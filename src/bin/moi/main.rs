// MOI command-line interface
#[macro_use] extern crate json;
extern crate mosquitto_client;
extern crate lapp;
extern crate toml;
extern crate md5;
extern crate libc;
extern crate ansi_term;
#[macro_use] extern crate log;
// our own common crate (shared with daemon)
#[macro_use]
extern crate moi;

mod strutil;
mod query;
mod flags;
mod commands;
// mod output;

use moi::*;
use moi::toml_utils::*;
use query::*;

use mosquitto_client::Mosquitto;
use json::JsonValue;
use ansi_term::{ANSIString,Colour,Style};
use Colour::{Red,Yellow,White};

use std::path::{Path,PathBuf};
use std::time::Duration;
use std::collections::HashMap;
use std::{fs,io,thread,process};
use std::io::prelude::*;
use std::error::Error;
use std::cell::Cell;

const LAUNCH_TIMEOUT:i32 = 20000;

const QUERY_TOPIC: &str = "MOI/query";
const QUERY_FILE_RESULT_TOPIC: &str = "MOI/result/query";
const QUERY_GROUP_RESULT_TOPIC: &str = "MOI/result/group";
const FILE_RESULT_TOPIC: &str = "MOI/result/file";
const PROCESS_RESULT_TOPIC: &str = "MOI/result/process";
const FILE_TOPIC_PREFIX: &str = "MOI/file";
const TIMEOUT_TOPIC: &str = "MOI/pvt/timeout";
const PROCESS_FETCH_TOPIC: &str = "MOI/fetch/";

pub fn json_out(cmd: &str, ok: bool, addr: &str, name: &str, j: JsonValue, cols: &[&str]) {
    print!("{{\"cmd\":{:?},\"ok\":{},\"addr\":{:?},\"name\":{:?}",cmd, ok, addr, name);
    if cols.len() > 0 {
        let mut res = String::from(",");
        for (e,col) in j.members().zip(cols) {
            let es = e.to_string();
            let es = if e.is_string() {
                format!("{:?}",es)
            } else {
                es
            };
            res.push_str(&format!("{:?}:{}",col,es));
            res.push(',');
        }
        res.pop();
        res.push('}');
        println!("{}",res);
    } else {
        println!("}}");
    }
}

struct MessageData {
    m: Mosquitto,
    sent_file: Option<String>, // can get this out of query, actually
    query: Vec<Query>,
    commands: Vec<flags::CommandArgs>,
    filter: Condition,
    all_group: JsonValue,
    maybe_group: Option<String>, // means group operation
    // used for (a) collecting during group command (b) existing group contents
    group: HashMap<String,String>,
    responses: HashMap<String,bool>,
    query_topic: String,
    finis: bool,
    seq: u8,
    flags: flags::Flags,
    no_groups: Cell<bool>

}

impl MessageData {
    fn new (m: &Mosquitto, flags: flags::Flags, commands: Vec<flags::CommandArgs>) -> MessageData {
        MessageData {
            m: m.clone(),
            sent_file: None,
            query: Vec::new(),
            commands: commands,
            filter: Condition::None,
            all_group: JsonValue::Null,
            maybe_group: None,
            group: HashMap::new(),
            responses: HashMap::new(),
            query_topic: QUERY_TOPIC.into(),
            finis: false,
            seq: 0,
            flags: flags,
            no_groups: Cell::new(false),
        }
    }

    // it's recommended to create a group called 'all' as soon as possible
    fn lookup_name(&self, id: &str) -> String {
        if self.all_group != JsonValue::Null {
            let res = &self.all_group[id];
            if res == &JsonValue::Null { return "<unknown>".into(); }
            res.to_string()
        } else {
            self.no_groups.set(true);
            "<unknown>".into()
        }
    }

    // given name, do reverse lookup for address in 'all' group
    fn lookup_addr(&self, name: &str) -> BoxResult<String> {
        if self.all_group != JsonValue::Null {
            // all matches!
            let addrs: Vec<_> = self.all_group.entries()
                .filter(|&(_,jname)| jname == name)
                .map(|(addr,_)| addr).collect();

            if addrs.len() == 0 {
                err_io(&format!("can't look up address of {}",name))
            } else
            if addrs.len() > 1 {
                err_io(&format!("multiple addresses for {}",name))
            } else {
                Ok(addrs[0].to_string())
            }
        } else {
            err_io("all group is not yet defined for lookup")
        }
    }

    // this is an operation over a specific group, so track
    // whether any group member is missing. Also faster
    // because we can bail out when everyone has replied.
    fn set_group(&mut self, name: &str, members: &JsonValue) {
        self.group = j_object_to_map(members);
        self.maybe_group = Some(name.into());
    }

    // this is an operation on a single device,
    // treated as a Group of One
    fn set_single_id(&mut self, addr: &str, was_addr: bool) -> BoxResult<()> {
        let addr = addr.to_string();
        let (addr,name) = if was_addr {
            let name = self.lookup_name(&addr);
            (addr,name)
        } else {
            let name = addr;
            let addr = self.lookup_addr(&name)?;
            (addr,name)
        };
        let group = object!{addr.as_str() => name.as_str()};
        self.set_group(&name,&group);
        // narrowcast - only want to bother one device
        self.query_topic = format!("{}/{}",QUERY_TOPIC,addr);
        Ok(())
    }

    // responses are coming in from remotes.
    // If we are filtering on a group, then we are
    // finished when we have collected all members.
    fn response(&mut self, id: String, ok: bool, handled: bool) {
        if ! ok && ! handled {
            let name = self.lookup_name(&id);
            error!("{} {} failed",id,name);
            if self.flags.json {
                json_out(&self.current_command().command,false,&id,&name,array!["failed"],&["error"]);
            }
        }
        if self.flags.verbose {
            println!("response {} {} {}",id,ok,handled);
        }
        self.responses.insert(id,ok);
        if self.maybe_group.is_some() {
            // not quite right ;)  Should check membership
            self.finis = self.responses.len() == self.group.len();
        }
    }

    // and then bail out
    fn group_finished(&self) -> bool {
        self.finis
    }

    fn current_query(&self) -> &Query {
        &self.query[self.seq as usize]
    }

    fn current_command(&self) -> &flags::CommandArgs {
        &self.commands[self.seq as usize]
    }

    fn set_queries(&mut self, q: Query) {
        // Actions is a vector of Queries!
        if let Query::Actions(queries) = q {
            self.query.extend(queries);
        } else {
            self.query.push(q);
        }
    }

    // these are the JSON-encoded responses from the devices
    fn parse_response(js: &str, rseq: &mut u8) -> (String,bool,JsonValue) {
        let mut j = match json::parse(js) {
            Ok(j) => j,
            Err(e) => return ("".into(), false, e.description().into())
        };
        // error handling....
        *rseq = j["seq"].as_u8().unwrap();
        let id = j["id"].to_string();
        if j["error"].is_null() {
            (id, true, j["ok"].take())
        } else {
            (id, false, j["error"].take())
        }
    }

    // how our JSON payload is encoded for remote queries
    fn send_query(&mut self) -> BoxResult<()> {
        if self.flags.verbose {
            println!("query {:?}",self.current_query());
        }
        info!("query seq {}: {:?}",self.seq,self.current_query());
        let q = self.current_query().to_json();
        self.responses.clear();
        if q == JsonValue::Null {
            return Ok(());
        }
        let mut q_json = object! {
            "seq" => self.seq,
            "which" => self.filter.to_json(),
            "what" => q,
        };
        if let Some(ref name) = self.maybe_group {
            q_json["group"] = name.as_str().into();
        }
        let payload = q_json.to_string();
        if self.flags.verbose {
            println!("sent {}",payload);
        }
        self.m.publish(&self.query_topic,payload.as_bytes(),1,false)?;
        Ok(())
    }

    fn bold<'a>(&self, s: &'a str, c: Colour) -> ANSIString<'a> {
        if self.flags.use_colour {
            c.bold().paint(s)
        } else {
            ANSIString::from(s)
        }
    }


    // result of a remote process is called either as a direct response (run)
    // or later (launch)
    fn handle_run_launch(&self, id: &str, resp: JsonValue) -> bool {
        let code = resp["code"].as_u32().unwrap();
        let stdout = resp["stdout"].to_string();
        let stderr = resp["stderr"].to_string();
        let output = if code == 0 {stdout} else {stderr};
        let name = self.lookup_name(id);
        if ! self.flags.json {
            let multiline = output.find('\n').is_some();
            let (delim,post) = if multiline {(":\n","\n")} else {("\t","")};
            if code == 0 {
                if ! self.flags.quiet {
                    println!("{}\t{}{}{}{}", self.bold(id,White),self.bold(&name,White),delim,output,post);
                }
                true
            } else {
                let code = code.to_string();
                println!("{}\t{}{}(code {}): {}{}", self.bold(id,White),self.bold(&name,White),delim
                    ,self.bold(&code,Red),output,post);
                // important: failed remote commands must count as failures
                false
            }
        } else {
            if self.flags.cols.len() > 0 && code == 0 {
                let columns = strings_to_json(output.split_whitespace());
                let colnames: Vec<_> = self.flags.cols.iter().map(|s| s.as_str()).collect();
                json_out("run",true,&id,&name,columns,&colnames);
            } else {
                json_out("run",code==0,&id,&name,array![code,output],&["code","output"]);
            }
            code == 0
        }
    }

    // comes in as MOI/fetch/{seq}/{addr}/{name}
    fn handle_fetch(&self, parms: &str, payload: &[u8], id: &mut String) -> BoxResult<String> {
        let mut iter = parms.split('/');
        let seq: u8 = iter.next().unwrap().parse()?;
        let addr = iter.next().unwrap();
        let name = iter.next().unwrap();
        *id = addr.into();

        (seq == self.seq)
            .or_then_err(|| format!("expected seq {}, got {}",self.seq,seq))?;
        let ff = match self.query[seq as usize] {
            Query::Fetch(ref ff) => ff,
            _ => {return err_io(&format!("MOI/fetch came in but not Fetch query!"));}
        };
        let local_dest = ff.local_dest.to_str().unwrap();
        if let Ok(dest) = strutil::replace_percent_destination(local_dest,addr,name) {
            let path = Path::new(&dest);
            if ff.pattern_dir { // e.g something like %n/file-name
                let parent = path.parent().unwrap();
                if ! parent.is_dir() {
                    fs::create_dir(parent)?;
                }
            }
            let mut f = fs::File::create(path)?;
            f.write_all(payload)?;
            return Ok(dest.clone());
        } else {
            return err_io(&format!("local dest substitution failed {}",ff.local_dest.display()));
        }
    }


    fn handle_response(&mut self, id: String, mut resp: JsonValue) {
        let mut ok = Some(true);
        let mut handled = false;
        let use_colour = self.flags.use_colour;
        let boldj = |j: &JsonValue| {
            let s = j.to_string();
            if use_colour {
                White.bold().paint(s)
            } else {
                Style::new().paint(s)
            }
        };
        // need a split borrow here, hence repeated code
        match self.query[self.seq as usize] {
            Query::Get(ref cols, ref command) => {
                match command.as_str() {
                    "ls" =>  {
                        if ! self.flags.json {
                            // Ugly. It will get Better...
                            let n = resp.len();
                            for idx in 0..n {
                                let r = &resp[idx];
                                if idx < 2 {
                                    print!("{}",boldj(r));
                                } else {
                                    if r == &JsonValue::Null {
                                        print!("{}",self.bold("null",Red));
                                    } else {
                                        print!("{}",r);
                                    }
                                }
                                if idx < n-1 {
                                    print!("\t");
                                }
                            }
                            println!();
                        } else {
                            // the result is _guaranteed_ to contain at least the address and the name
                            let addr = resp.array_remove(0);
                            let name = resp.array_remove(0);
                            let cols: Vec<_> = cols.iter().skip(2).map(|s| s.as_str()).collect();
                            json_out("ls",true,as_str_always(&addr),as_str_always(&name),resp,&cols);
                        }
                    },
                    "time" => {
                        let id = &resp[0];
                        let name = &resp[1];
                        let time = resp[2].as_i64().unwrap();
                        let now = current_time_as_secs();
                        let diff = now - time;
                        if ! self.flags.json {
                            println!("{}\t{}\t{}",boldj(id),boldj(name),diff);
                        } else {
                            json_out("time",true,as_str_always(id),as_str_always(name),array![diff],&["time diff"]);
                        }
                    },
                    _ => {}
                }
            },
            Query::Ping(instant) => {
                // also a Get operation under the hood...
                if ! self.flags.quiet {
                    let id = &resp[0];
                    let name = &resp[1];
                    let diff = duration_as_millis(instant.elapsed()) as i32;
                    if ! self.flags.json {
                        println!("{}\t{}\t{}",boldj(id),boldj(name),diff);
                    } else {
                        json_out("ping",true,as_str_always(id),as_str_always(name),array![diff],&["ping"]);
                    }
                }
            },
            Query::Invoke(_,_) => {
                let name = self.lookup_name(&id);
                println!("{}\t{}\t{}",id,name,resp);
            },
            Query::Group(_,_) => {
                // a Get operation for collecting group members
                let get = &resp[0];
                self.group.insert(get[0].to_string(),get[1].to_string());
            },
            Query::Run(_) => {
                ok = Some(self.handle_run_launch(&id,resp));
                handled = true;
            },
            Query::Fetch(_) => {
                // note: not currently used...
                // contents coming over as MOI/fetch/{seq}/{addr}/{name}
                ok = None;
            },
            Query::Copy(ref cf) => {
                // the first response we get, we post the actual file contents
                if self.sent_file.is_none() {
                    let bytes = &cf.bytes;
                    let topic = format!("{}/{}",FILE_TOPIC_PREFIX,self.seq);
                    if self.flags.verbose {
                        println!("publishing {} {} bytes on {}",cf.filename,bytes.len(),topic);
                    }
                    self.m.publish(&topic,bytes,1,true).unwrap();
                    self.sent_file = Some(topic);
                }
                ok = None;
            },
            _ => { }
        }
        if let Some(ok) = ok {
            self.response(id,ok,handled);
        }
    }

    fn finish_off(&mut self, store: &mut Config) -> BoxResult<bool> {
        if self.no_groups.get() {
            warn!("all group not defined yet: say 'moi group all'");
        }
        Ok(if let Query::Group(ref name, _) = *self.current_query() {
            // the group command collects group members
            // which we then persist to file
            // TODO: error checking
            if ! self.flags.json {
                warn!("group {} created:",name);
                for (k,v) in &self.group {
                    println!("{}\t{}",self.bold(k.as_str(),White),self.bold(v.as_str(),White));
                }
            } else {
                for (k,v) in &self.group {
                    json_out("group",true,k,v,array![name.as_str()],&["group"]);
                }
            }
            let jg = to_jobject(&self.group);
            { // NLL !
                let groups = store.values.entry("groups".to_string())
                    .or_insert_with(|| JsonValue::new_object());
                groups[name] = jg;
            }
            store.write()?;
            true
        } else
        if let Some(ref group_name) = self.maybe_group {
            // Group filters rely on special array-based key 'groups', _plus_
            // group responses are checked against saved group members
            let group = &self.group;
            let responses = &self.responses;
            let mut ok = true;
            for (id,success) in responses {
                if let None = group.get(id) {
                    println!("note: id {} not in group {}", id, group_name);
                }
                if ! success {
                    ok = false;
                }
            }
            for (id,name) in group {
                if ! responses.contains_key(id) {
                    error!("error: {} {} failed to respond", id, name);
                    if self.flags.json {
                        json_out(&self.current_command().command,false,id,name,array!["failed to respond"],&["error"]);
                    }
                    ok = false;
                }
            }
            ok
        } else {
            self.responses.iter().all(|(_,&ok)| ok)
        })
    }

    fn process_flags(&mut self, store: &Config) -> BoxResult<()> {
        // --name: this can be an address, name or group!
        if self.flags.name_or_group != "none" {
            if strutil::is_ipv4(&self.flags.name_or_group) {
                self.flags.filter_desc = format!("addr={}",self.flags.name_or_group);
            } else {  // a name?
                if let Ok(addr) = self.lookup_addr(&self.flags.name_or_group) {
                    self.flags.filter_desc = format!("addr={}",addr);
                } else {
                    self.flags.group_name = self.flags.name_or_group.clone();
                }
            }
        }

        // --group NAME works like --filter groups:NAME
        // except the results are checked against saved group information
        if self.flags.group_name != "none" {
            let jgroup = lookup_group(&store, &self.flags.group_name)?;
            // multistage group commands stop at first non-sucessful run operation
            self.flags.filter_desc = format!("all groups:{} rc=0 {}",
                self.flags.group_name,
                if self.flags.filter_desc != "none" {self.flags.filter_desc.as_str()} else {""}
            );
            let group_name = self.flags.group_name.clone();
            self.set_group(&group_name,jgroup);
        }
        Ok(())
    }

}

fn lookup_group<'a>(store: &'a Config, group_name: &str) -> io::Result<&'a JsonValue> {
    let groups = store.values.get("groups").or_err("no groups defined!")?;
    let jgroup = &groups[group_name];
    (jgroup.is_object()).or_err("no such group")?;
    Ok(jgroup)
}

// our real error-returning main function.
fn run() -> BoxResult<bool> {
    let (commands,mut flags) = flags::Flags::new()?;
    let toml: toml::Value = read_to_string(&flags.config_file)?.parse()?;
    let config = toml.get("config").or_err("No [config] section")?;

    let path: PathBuf = if let Some(log_file) = gets_opt(config,"log_file")? {
        log_file.into()
    } else {
        flags.moi_dir.join("moi.log")
    };
    // we DON'T log non-su moi invocations if there's a su install
    let log_file = if flags.sharing_with_su { None } else { Some(path.as_path()) };
    // we echo errors and warnings to console with colours (unless JSON output)
    let echo_console = ! flags.json;
    let use_colour = flags.use_colour;
    logging::init(log_file,gets_or(config,"log_level","info")?,move |record| {
        if echo_console {
            if record.level() == log::Level::Error {
                if use_colour {
                    let text = format!("{}",record.args());
                    eprintln!("{}",Red.bold().paint(text));
                } else {
                    eprintln!("{}",record.args());
                }
            }  else
            if record.level() == log::Level::Warn {
                if use_colour {
                    let text = format!("{}",record.args());
                    println!("{}",Yellow.bold().paint(text));
                } else {
                    println!("{}",record.args());
                }
            }
        }
    })?;

    let json_store = match gets_opt(config,"store")? {
        Some(s) => PathBuf::from(s),
        None => flags.json_store.clone()
    };
    let mut store = Config::new_from_file(&config, &json_store)?;

    {
        let cmds = commands::CommandHandler::new(&flags,&store,&toml);
        match commands[0].command.as_str() {
            "groups" => {
                return cmds.groups();
            },
            "commands" => {
                return cmds.custom_commands();
            },
            "setup" => { // dummy command (MAY become required)
                return Ok(true);
            },
            _ => {}
        }
    }

    let m = mosquitto_setup("moi",&config,&toml,flags.moi_dir.join("certs"))?;

    let query_resp = m.subscribe(QUERY_FILE_RESULT_TOPIC,1)?;
    let group_resp = m.subscribe(QUERY_GROUP_RESULT_TOPIC,1)?;
    let file_resp = m.subscribe(FILE_RESULT_TOPIC,1)?;
    let pvt_timeout = m.subscribe(TIMEOUT_TOPIC,1)?;
    m.subscribe(&(PROCESS_FETCH_TOPIC.to_string() + "#"),1)?;
    let process_resp = m.subscribe(PROCESS_RESULT_TOPIC,1)?;

    // parse the command and create a Query
    // This looks up any command aliases and may modify
    // flags.group or flags.filter
    // By default, ordinary moi users are restricted in what commands
    // they can execute - basically just forms of 'ls'. However,
    // command aliases in the moi superuser directory are not restricted.
    let restricted = if flags.su {
        false
    } else {
        gets_or(&config,"restricted","yes")? == "yes"
    };
    let query = flags.construct_query_alias(&toml, &config, &commands, restricted)?;

    // message data is managed by mosquitto on_message handler
    let mut message_data = MessageData::new(&m,flags,commands);

    message_data.all_group = match store.values.get("groups") {
        Some(groups) => {
            groups["all"].clone()
        },
        None => JsonValue::Null
    };
    message_data.set_queries(query);

    message_data.process_flags(&store)?;

    let filter = Condition::from_description(&message_data.flags.filter_desc);
    info!("filter {:?}",filter);

    // Queries only meant for one device cause a temporary group
    // to be created.
    if let Some((id,was_addr)) = filter.unique_id() {
        message_data.set_single_id(&id,was_addr)?;
    }
    message_data.filter = filter;

    let launching = message_data.query.iter().any(|q| q.is_wait());
    if launching && message_data.maybe_group.is_none() {
        warn!("Warning: no group defined for wait! Setting timeout to {}ms",LAUNCH_TIMEOUT);
    }

    let timeout = timeout::Timeout::new_shared(message_data.flags.timeout);

    let msg_timeout = timeout.clone();
    let mut mc = m.callbacks(message_data);
    mc.on_message(|data,msg| {
        lock!(msg_timeout).update(); // feed the watchdog
        if query_resp.matches(&msg) {
            let mut seq = 0;
            let (id,success,resp) = MessageData::parse_response(msg.text(),&mut seq);
            if ! success {
                let name = data.lookup_name(&id);
                error!("{}\t{}\t{}", id,name,resp);
                if data.flags.json {
                    json_out(&data.current_command().command,false,&id,&name,array![resp],&["error"]);
                }
                data.response(id,false,true);
                return;
            } else {
                if data.flags.verbose {
                    println!("id {} resp {}", id,resp);
                }
                info!("seq {} addr {} resp {}", seq,id,resp);
                if seq != data.seq {
                    error!("late arrival {} {}: seq {} != {}",id,data.lookup_name(&id),seq,data.seq);
                } else {
                    data.handle_response(id,resp);
                }
            }
        } else
        if file_resp.matches(&msg) {
            let mut seq = 0;
            let (id,ok,_) = MessageData::parse_response(msg.text(),&mut seq);
            if data.flags.json && ok {
                let file = data.current_command().arguments[0].as_str();
                json_out("push",true,&id,&data.lookup_name(&id),array![file],&["file"]);
            }
            data.response(id,ok,false);
        } else
        if process_resp.matches(&msg) {
            let mut seq = 0;
            let (id,mut ok,resp) = MessageData::parse_response(msg.text(),&mut seq);
            let mut handled = false;
            if ok {
                // a commmand was executed which may have failed
                // either way, we report the result
                handled = true;
                ok = data.handle_run_launch(&id, resp);
            }
            data.response(id,ok,handled);
        } else
        if msg.topic().starts_with(PROCESS_FETCH_TOPIC) {
            let parms = &(msg.topic())[PROCESS_FETCH_TOPIC.len()..];
            let mut id = String::new();
            match data.handle_fetch(parms,msg.payload(),&mut id) {
                Err(e) => {
                    error!("pull error {} {}", id,e);
                },
                Ok(dest) => {
                    if data.flags.json {
                        let file = data.current_command().arguments[0].as_str();
                        json_out("pull",true,&id,&data.lookup_name(&id),
                            array![file,dest.as_str()],&["remote","local"]);
                    }
                }
            }
            data.response(id,true,false);
        } else
        if group_resp.matches(&msg) {
            // group operations always notify us back
            let id = msg.text();
            data.response(id.into(),true,false);
        }

        if data.group_finished() || pvt_timeout.matches(&msg) {
            // TOO MANY UNWRAPS!
            if data.flags.verbose { println!("timeout seq {} {}",data.seq,data.query.len()); }
            // clear any retained file content messages
            let mut sent_file = false;
            if let Some(ref file_topic) = data.sent_file {
                sent_file = true;
                m.publish(file_topic,b"",1,true).unwrap();
                m.do_loop(50).unwrap(); // ensure it's actually published
                if data.flags.verbose { println!("clearing file topic {}",file_topic); }
            }
            if sent_file {
                data.sent_file = None;
            }
            if data.seq as usize == data.query.len()-1 {
                // bail out, our business is finished
                if let Err(e) = m.disconnect() {
                    eprintln!("disconnect error {}",e);
                    process::exit(1);
                }
            } else {
                // aha, there's another query in the pipeline...
                data.seq += 1;
                // Wait has VERY generous timeout...
                if data.current_query().is_wait() {
                    lock!(msg_timeout).set_timeout(LAUNCH_TIMEOUT);
                }
                data.send_query().unwrap();
            }
        }
    });

    // now that we're listening for a response, send the query...
    mc.data.send_query()?;

    // our basic timeout Watchdog - if messages haven't arrived
    // within a timeout period, we disconnect.
    let thread_m = m.clone();
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_millis(50));
            if lock!(timeout).timed_out() {
                // errors! Should bail out more elegantly here...
                thread_m.publish(TIMEOUT_TOPIC,b"",1,false).unwrap();
            }
        }
    });


    m.loop_until_disconnect(-1)?;

    let ok = mc.data.finish_off(&mut store)?;

    Ok(ok)
}

fn main() {
    match run() {
        Ok(ok) => {
            if ! ok {
                process::exit(1);
            }
        },
        Err(e) => {
            let text = format!("error: {}",e);
            eprintln!("{}",Red.bold().paint(text));
        }
    }
}
