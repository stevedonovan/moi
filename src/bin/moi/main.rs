// MOI command-line interface
#[macro_use] extern crate json;
extern crate mosquitto_client;
extern crate lapp;
extern crate toml;
// our own common crate (shared with daemon)
#[macro_use]
extern crate moi;

use moi::*;

mod strutil;
mod commands;
mod toml_utils;
mod timeout;
// mod output;

use commands::*;
use toml_utils::*;

use mosquitto_client::Mosquitto;
use json::JsonValue;

use std::path::{Path,PathBuf};
use std::time::{Instant,Duration};
use std::collections::HashMap;
use std::thread;
use std::{fs,env,io};
use std::io::prelude::*;
//use std::fs::File;
use std::error::Error;

const VERSION: &str = "0.1.1";
const LAUNCH_TIMEOUT:i32 = 20000;

const USAGE: &str = "
Execute commands on devices
  -V, --version version of MOI
  -c, --config (path default ~/.local/moi/config.toml) configuration file
  -f, --filter (default none) only for the selected devices
            KEY test for existence of key
            KEY=VALUE  test for equality
            KEY=VAL#   test for values that start with given string
            KEY:VALUE  test whether value is in the array KEY
            KEY.not.VALUE test for values not equal to VALUE
  -g, --group (default none) for a predefined group
  -T, --timeout (default 300) timeout for accessing all devices
  -v, --verbose tell us all about what's going on...
  -q, --quiet output only on error
  -m, --message-format (default plain) one of plain,csv or json
  <command> (string)
        ls <keys>: display values of keys (defaults to 'addr','name')
        run cmd [pwd]: run command remotely
        launch cmd [pwd]: like run - use instead when command can take a long time
        push file dest: copy a file to a remote destination
        push-run file dest cmd: copy a file and run a command
        pull file dest: copy remote files to us
        run-pull cmd file dest: run a command and then copy the result
        set key=value...:  set keys on remotes
        seta key=value...: append values to array-valued keys
        group name: create a group from the set of responses
        remove-group: remove a named group from the set
        groups: show defined groups
        ping:  like ls, but gives round-trip time in msec
        time:  like ls, but gives difference between this time and device time, in secs
  <args> (string...) additional arguments for commands
";

#[derive(Clone)]
struct CommandArgs {
    command: String,
    arguments: Vec<String>,
}

struct Flags {
    filter_desc: String,
    group_name: String,
    config_file: PathBuf,
    moi_dir: PathBuf,
    json_store: PathBuf,
    timeout: i32,
    verbose: bool,
    quiet: bool,
   // format: String,
}

impl Flags {
    fn new(args: &lapp::Args) -> BoxResult<(Vec<CommandArgs>,Flags)> {
        if args.get_bool("version") {
            println!("MOI comand-line interface version {}",VERSION);
            std::process::exit(0);
        }

        let moi_dir = env::home_dir().unwrap().join(".local").join("moi");
        let default_config = moi_dir.join("config.toml");
        let json_store = moi_dir.join("store.json");
        if ! moi_dir.exists() {
            fs::create_dir_all(&moi_dir)?;
            write_all(&default_config,"[config]\nmqtt_addr = \"localhost\"\n")?;
            write_all(&json_store,"{}\n")?;
            println!("Creating {}.\nEdit mqtt_addr if necessary",default_config.display());
        }

        let command = args.get_string("command");
        let mut arguments = args.get_strings("args");
        arguments.insert(0,command);

        let mut commands = Vec::new();
        {
            let mut push = |mut aa: Vec<String>| {
                if aa.len() == 0 { args.quit("must have at least one value after ::"); }
                commands.push(CommandArgs{command: aa.remove(0), arguments: aa});
            };
            let mut this_chunk = Vec::new();
            for s in arguments {
                if s == "::" {
                    let mut tmp = Vec::new();
                    std::mem::swap(&mut this_chunk, &mut tmp);
                    push(tmp);
                    // this_chunk is now a new empty vector
                } else {
                    this_chunk.push(s);
                }
            }
            push(this_chunk);
        }

        Ok((commands,Flags {
            filter_desc: args.get_string("filter"),
            group_name: args.get_string("group"),
            timeout: args.get_integer("timeout"),
            verbose: args.get_bool("verbose"),
            quiet: args.get_bool("quiet"),
            config_file: args.get_path("config"),
            json_store: json_store,
            moi_dir: moi_dir,
     //       format: args.get_string("message_format"),
        }))

    }
}

const QUERY_TOPIC: &str = "MOI/query";
const QUERY_FILE_RESULT_TOPIC: &str = "MOI/result/query";
const FILE_RESULT_TOPIC: &str = "MOI/result/file";
const PROCESS_RESULT_TOPIC: &str = "MOI/result/process";
const FILE_TOPIC_PREFIX: &str = "MOI/file";
const TIMEOUT_TOPIC: &str = "MOI/pvt/timeout";
const PROCESS_FETCH_TOPIC: &str = "MOI/fetch/";

struct MessageData {
    m: Mosquitto,
    sent_file: Option<String>, // can get this out of query, actually
    query: Vec<Query>,
    filter: Condition,
    all_group: JsonValue,
    maybe_group: Option<String>, // means group operation
    // used for (a) collecting during group command (b) existing group contents
    group: HashMap<String,String>,
    responses: HashMap<String,bool>,
    query_topic: String,
    finis: bool,
    seq: u8,
    verbose: bool,
    quiet: bool,
   // formatter: output::Output,
}

impl MessageData {
    fn new (m: &Mosquitto, verbose: bool, quiet: bool) -> MessageData { // , formatter: output::Output
        MessageData {
            m: m.clone(),
            sent_file: None,
            query: Vec::new(),
            filter: Condition::None,
            all_group: JsonValue::Null,
            maybe_group: None,
            group: HashMap::new(),
            responses: HashMap::new(),
            query_topic: QUERY_TOPIC.into(),
            finis: false,
            seq: 0,
            verbose: verbose,
            quiet: quiet,
            //formatter: formatter
        }
    }

    // it's recommended to create a group called 'all' as soon as possible
    fn lookup_name(&self, id: &str) -> String {
        if self.all_group != JsonValue::Null {
            let res = &self.all_group[id];
            if res == &JsonValue::Null { return "<unknown>".into(); }
            res.to_string()
        } else {
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

    // this is an operation on a single device
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
            eprintln!("{} {} failed",id,self.lookup_name(&id));
        }
        if self.verbose {
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
        if self.verbose {
            println!("query {:?}",self.current_query());
        }
        let q = self.current_query().to_json();
        self.responses.clear();
        if q == JsonValue::Null {
            return Ok(());
        }
        let q_json = object! {
            "seq" => self.seq,
            "which" => self.filter.to_json(),
            "what" => q,
        };
        let payload = q_json.to_string();
        if self.verbose {
            println!("sent {}",payload);
        }
        self.m.publish(&self.query_topic,payload.as_bytes(),1,false)?;
        Ok(())
    }

    // result of a remote process is called either as a direct response (run)
    // or later (launch)
    fn handle_run_launch(&self, id: &str, resp: JsonValue) -> bool {
        let code = resp["code"].as_u32().unwrap();
        let stdout = resp["stdout"].to_string();
        let stderr = resp["stderr"].to_string();
        let output = if code == 0 {stdout} else {stderr};
        let multiline = output.find('\n').is_some();
        let (delim,post) = if multiline {(":\n","\n")} else {("\t","")};
        let name = self.lookup_name(id);
        if code == 0 {
            if ! self.quiet {
                println!("{}\t{}{}{}{}",id,name,delim,output,post);
            }
            true
        } else {
            println!("{}\t{}{}(code {}): {}{}",id,name,delim,code,output,post);
            // important: failed remote commands must count as failures
            false
        }
    }

    // comes in as MOI/fetch/{seq}/{addr}/{name}
    fn handle_fetch(&self, parms: &str, payload: &[u8], id: &mut String) -> BoxResult<()> {
        let mut iter = parms.split('/');
        let seq: u8 = iter.next().unwrap().parse()?;
        let addr = iter.next().unwrap();
        let name = iter.next().unwrap();
        //println!("DBG: pull got {} {} {}",seq,addr,name);
        *id = addr.into();

        (seq == self.seq)
            .or_then_err(|| format!("expected seq {}, got {}",self.seq,seq))?;
        let ff = match self.query[seq as usize] {
            Query::Fetch(ref ff) => ff,
            _ => {return err_io(&format!("MOI/fetch came in but not Fetch query!"));}
        };
        if let Ok(dest) = strutil::replace_percent_destination(ff.local_dest.to_str().unwrap(),addr,name) {
            let mut f = fs::File::create(&dest)?;
            f.write_all(payload)?;
        } else {
            return err_io(&format!("local dest subsitution failed {}",ff.local_dest.display()));
        }
        Ok(())
    }


    fn handle_response(&mut self, id: String, resp: JsonValue) {
        let mut ok = Some(true);
        let mut handled = false;
        // need a split borrow here, hence repeated code
        match self.query[self.seq as usize] {
            Query::Get(_, ref command) => {
                match command.as_str() {
                    "ls" =>  {
                        // Ugly. It will get Better...
                        let n = resp.len();
                        for idx in 0..n {
                            let r = &resp[idx];
                            print!("{}",r);
                            if idx < n-1 {
                                print!("\t");
                            }
                        }
                        println!();
                    },
                    "time" => {
                        let time = resp[2].as_i64().unwrap();
                        let now = current_time_as_secs();
                        println!("{}\t{}\t{}",resp[0],resp[1],now - time);
                    },
                    _ => {}
                }
            },
            Query::Ping(instant) => {
                // also a Get operation under the hood...
                if ! self.quiet {
                    let diff = duration_as_millis(instant.elapsed()) as u32;
                    println!("{}\t{}\t{}",resp[0],resp[1],diff);
                }
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
            //~ Query::Launch(_) => {
                //~ // remote is saying 'fine I've launched process. Be patient'
                //~ ok = None;
            //~ },
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
                    if self.verbose {
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
        Ok(if let Query::Group(ref name, _) = *self.current_query() {
            // the group command collects group members
            // which we then persist to file
            // TODO: error checking
            println!("group {} created:",name);
            for (k,v) in &self.group {
                println!("{}\t{}",k,v);
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
            // Group filters rely on special key 'group', _plus_
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
                    eprintln!("error: {} {} failed to respond", id, name);
                    ok = false;
                }
            }
            ok
        } else {
            self.responses.iter().all(|(_,&ok)| ok)
        })
    }

}

// implement our commands as Query enum values
fn construct_query(cmd: &str, args: &[String]) -> BoxResult<Query> {
    use strutil::strings;
    match cmd {
        "ls" => {
            Ok(Query::get(args.to_vec(),cmd.into()))
        },
        "time" => {
            Ok(Query::Get(vec!["addr".into(),"name".into(),"time".into()],cmd.into()))
        },
        "ping" => {
            Ok(Query::Ping(Instant::now()))
        },
        "group" => {
            (args.len() == 1).or_err("group: group-name")?;
            Ok(Query::group(&args[0]))
        },
        "set" | "seta" => {
            (args.len() > 0).or_err("set: key1=value1 [key2=value2 ...]")?;
            let mut map = HashMap::new();
            for s in args {
                let (k,v) = strutil::split_at_delim(s,"=")
                    .or_then_err(|| format!("{} is not a key-value pair",s))?;
                KeyValue::valid_key(k)
                    .or_then_err(|| format!("{} is not a valid key name",k))?;
                map.insert(k.to_string(),v.to_string());
            }
            Ok(if cmd=="set" {Query::Set(map)} else {Query::Seta(map)})
        },
        "remove-group" => {
            (args.len() == 1).or_err("remove-group: group-name")?;
            Ok(Query::rma("groups",&args[0]))
        },
        "run" | "launch" | "spawn" => {
            (args.len() >= 1).or_err("run: command [working-dir] [job-name]")?;
            let rc = RunCommand::new(&args[0],args.get(1).cloned(),args.get(2).cloned());
            Ok(
                if cmd=="run" {Query::Run(rc)}
                else if cmd=="launch" {Query::Launch(rc)}
                else {Query::Spawn(rc)}
            )
        },
        "wait" => Ok(Query::Wait),
        "push" => {
            (args.len() == 2).or_err("push: local-file-name remote-dest")?;
            let path = PathBuf::from(args[0].clone());
            (path.exists() && path.is_file()).or_err("push: file does not exist, or is a directory")?;
            let mut cf = CopyFile::new(
                path,
                &args[1]
            );
            cf.read_bytes()?;
            Ok(Query::Copy(cf))
        },
        "pull" => {
            (args.len() == 2).or_err("pull: remote-file-name local-dest")?;
            let remote_path = PathBuf::from(args[0].clone());
            let local_path = PathBuf::from(args[1].clone());
            (! local_path.is_dir())
                .or_then_err(|| format!("pull: destination {} must not be a directory!",local_path.display()))?;
            {
                let parent = local_path.parent()
                    .or_then_err(|| format!("pull: destination {} has no parent",local_path.display()))?;
                if parent != Path::new("") {
                    writeable_directory(&parent)?;
                }
            }
            Ok(Query::Fetch(FetchFile {
                source: remote_path,
                local_dest: local_path,
            }))
        },
        "push-run" => {
            // example of a two-step command
            (args.len() == 3).or_err("push-run: local-file destination command")?;
            let file = &args[0];
            let dest = &args[1];
            let cmd = &args[2];
            Ok(Query::Actions(vec![
                construct_query("push",&strings(&[file,dest]))?,
                construct_query("run",&strings(&[cmd,dest]))? // use dest as pwd
            ]))
        },
        "run-pull" => {
            (args.len() == 3).or_err("run-pull: command dir remote-file")?;
            let cmd = &args[0];
            let dir = &args[1];
            let file = &args[2];
            Ok(Query::Actions(vec![
                construct_query("run",&strings(&[cmd,dir]))?,
                construct_query("pull",&strings(&[file,dir]))?
            ]))
        },
        "restart" => {
            Ok(Query::Restart(0))
        },
        _ => {
            err_io(&format!("not a command: {}",cmd))
        }
    }
}

fn query_alias(def: &toml::Value, flags: &Flags, cmd: &CommandArgs, help: &str) -> BoxResult<Query> {
    // MUST have at least "command" and "args"
    let alias_command = def.get("command").or_err("alias: command must be defined")?
        .as_str().or_err("alias: command must be string")?;

    let alias_args = toml_strings(def.get("args").or_err("alias: args must be defined")?
        .as_array().or_err("alias: args must be array")?
    )?;
    let alias_args = strutil::replace_dollar_args_array(&alias_args,&cmd.arguments)
        .map_err(|e| io_error(&format!("{} {} {}",cmd.command, help, e)))?;

    if flags.verbose {
        println!("alias command {} args {:?}",alias_command,alias_args);
    }
    Ok(construct_query(alias_command,&alias_args)?)
}

fn query_alias_collect(t: &toml::Value, flags: &mut Flags, cmd: &CommandArgs, res: &mut Vec<Query>) -> BoxResult<()> {
    // either the filter or the group can be overriden, but currently only in the first command
    // of a sequence
    if let Some(filter) = gets(t,"filter")? {
        flags.filter_desc = filter.into();
    } else
    if let Some(group) = gets(t,"group")? {
        flags.group_name = group.into();
    }
    if let Some(_) = t.get("quiet") {
        flags.quiet = true;
    }

    // it's a cool thing to help people.
    let help = gets_or(t,"help","<no help>")?;
    // there may be multiple stages, so sections [commands.NAME.1], [commands.NAME.2]... etc in config
    let stages = geti_or(t,"stages",0)?;
    if stages == 0 {
        res.push(query_alias(t,flags,cmd,help)?);
    } else {
        for i in 1..stages+1 {
            let idx = i.to_string();
            let sub = t.get(&idx).or_then_err(|| format!("stage {} not found",idx))?;
            res.push(query_alias(sub,flags,cmd,help)?);
        }
    }
    Ok(())
}

// Program arguments passed as mutable reference, because
// command aliases MAY modify the filter or group value
fn construct_query_alias(aliases: Option<&toml::Value>, commands: &[CommandArgs], flags: &mut Flags) -> BoxResult<Query> {
    let mut res = Vec::new();
    for cmd in commands.iter() {
        let mut was_alias = false;
        // there is a section [commands.NAME] in the config TOML
        if let Some(ref lookup) = aliases {
            if let Some(t) = lookup.get(&cmd.command) { // we have an alias!
                query_alias_collect(t,flags,cmd,&mut res)?;
                was_alias = true;
            }
        }
        // OK, maybe the command NAME is NAME.toml or ~/.moi/NAME.toml
        if ! was_alias {
            if let Some(toml) = maybe_toml_config(&cmd.command,&flags.moi_dir)? {
                query_alias_collect(&toml,flags,cmd,&mut res)?;
                was_alias = true;
            }
        }
        // regular plain jane arguments - will complain if not recognized
        if ! was_alias {
            res.push(construct_query(&cmd.command, &cmd.arguments)?)
        }
    }

    // we can pack multiple queries into Actions,
    // but pass through single queries as is
    Ok(if res.len() == 1 {
        res.remove(0)
    } else {
        Query::Actions(res)
    })
}

fn lookup_group<'a>(store: &'a Config, group_name: &str) -> io::Result<&'a JsonValue> {
    let groups = store.values.get("groups").or_err("no groups defined!")?;
    let jgroup = &groups[group_name];
    (jgroup.is_object()).or_err("no such group")?;
    Ok(jgroup)
}

fn cat(a: &str, b: &str) -> String {
    a.to_string() + b
}

// our real error-returning main function.
fn run() -> BoxResult<bool> {
    let lapp_args = lapp::parse_args(USAGE);
    let (commands,mut flags) = Flags::new(&lapp_args)?;
    let toml: toml::Value = read_to_string(&flags.config_file)?.parse()?;
    let config = toml.get("config").or_err("No [config] section")?;
    config.is_table().or_err("config must be a table")?;
    let command_aliases = toml.get("commands");

    let mut store = Config::new_from_file(&flags.json_store)?;

    if commands[0].command == "groups" {
        if let Some(groups) = store.values.get("groups") {
            for (name,members) in groups.entries() {
                if flags.verbose {
                    println!("{}:",name);
                    for (addr,name) in members.entries() {
                        println!("\t{}\t{}",addr,name);
                    }
                } else {
                    println!("{} {} members",name,members.len());
                }
            }
        } else {
            println!("no groups yet!");
        }
        return Ok(true);
    }

    let m = mosquitto_client::Mosquitto::new("moi");

    m.connect_wait(
        gets_or(config,"mqtt_addr","127.0.0.1")?,
        geti_or(config,"mqtt_port",1883)? as u32,
        geti_or(config,"mqtt_connect_wait",300)? as i32
    )?;

    let query_resp = m.subscribe(QUERY_FILE_RESULT_TOPIC,1)?;
    let file_resp = m.subscribe(FILE_RESULT_TOPIC,1)?;
    let pvt_timeout = m.subscribe(TIMEOUT_TOPIC,1)?;
    m.subscribe(&cat(PROCESS_FETCH_TOPIC,"#"),1)?;
    let process_resp = m.subscribe(PROCESS_RESULT_TOPIC,1)?;

    // parse the command and create a Query
    // This looks up any command aliases and may modify
    // flags.group or flags.filter
    let query = construct_query_alias(command_aliases, &commands, &mut flags)?;

    // message data is managed by mosquitto on_message handler
    let mut message_data = MessageData::new(&m,flags.verbose,flags.quiet);
    message_data.all_group = match store.values.get("groups") {
        Some(groups) => {
            groups["all"].clone()
        },
        None => JsonValue::Null
    };
    message_data.set_queries(query);

    // --group NAME works like --filter groups:NAME
    // except the results are checked against saved group information
    if flags.group_name != "none" {
        if flags.filter_desc != "none" {
            println!("note: ignoring --filter when --group is present");
        }
        let jgroup = lookup_group(&store, &flags.group_name)?;
        flags.filter_desc = format!("all groups:{} rc=0",flags.group_name);
        message_data.set_group(&flags.group_name,jgroup);
    }
    let filter = Condition::from_description(&flags.filter_desc);
    // Queries only meant for one device cause a temporary group
    // to be created.
    if let Some((id,was_addr)) = filter.unique_id() {
        message_data.set_single_id(&id,was_addr)?;
    }
    message_data.filter = filter;

    let launching = message_data.query.iter().any(|q| q.is_wait());
    if launching && message_data.maybe_group.is_none() {
        println!("Warning: no group defined for wait! Setting timeout to {}ms",LAUNCH_TIMEOUT);
    }

    let timeout = timeout::Timeout::new_shared(flags.timeout);

    let msg_timeout = timeout.clone();
    let mut mc = m.callbacks(message_data);
    mc.on_message(|data,msg| {
        lock!(msg_timeout).update(); // feed the watchdog
        if query_resp.matches(&msg) {
            let mut seq = 0;
            let (id,success,resp) = MessageData::parse_response(msg.text(),&mut seq);
            if ! success {
                eprintln!("error for {} seq {}: {}",id,seq,resp.to_string());
                data.response(id,false,false);
                return;
            } else {
                if data.verbose {
                    println!("id {} resp {}",id, resp.to_string());
                }
                if seq != data.seq {
                    eprintln!("late arrival {}: seq {} != {}",id,seq,data.seq);
                } else {
                    data.handle_response(id,resp);
                }
            }
        } else
        if file_resp.matches(&msg) {
            let mut seq = 0;
            let (id,ok,_) = MessageData::parse_response(msg.text(),&mut seq);
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
            if let Err(e) = data.handle_fetch(parms,msg.payload(),&mut id) {
                eprintln!("pull error: {}",e);
                std::process::exit(1);
            }
            data.response(id,true,false);
        }

        if data.group_finished() || pvt_timeout.matches(&msg) {
            // TOO MANY UNWRAPS!
            if data.verbose { println!("timeout seq {} {}",data.seq,data.query.len()); }
            // clear any retained file content messages
            if let Some(ref file_topic) = data.sent_file {
                m.publish(file_topic,b"",1,true).unwrap();
                m.do_loop(50).unwrap(); // ensure it's actually published
            }
            if data.seq as usize == data.query.len()-1 {
                // bail out, our business is finished
                if let Err(e) = m.disconnect() {
                    println!("disconnect error {}",e);
                    std::process::exit(1);
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
                std::process::exit(1);
            }
        },
        Err(e) => {
            eprintln!("error: {}",e);
        }
    }
}
