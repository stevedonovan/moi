// MOI remote daemon
#[macro_use] extern crate log;
#[macro_use] extern crate json;
#[macro_use] extern crate moi;
extern crate toml;
extern crate mosquitto_client;
extern crate md5;
extern crate libc;

mod plugin;
use plugin::Plugins;

const VERSION: &str = "0.1.4";

use mosquitto_client::{Mosquitto,MosqMessage};
use json::JsonValue;

use moi::*;
use moi::toml_utils::*;

// we don't do Windows for now, sorry
use std::os::unix::fs::OpenOptionsExt;
use std::{io,fs,env};
use std::io::prelude::*;
use std::path::{Path,PathBuf};
use std::thread;
use std::time;
use std::process;

//use std::collections::HashMap;
use std::error::Error;

const QUERY_TOPIC: &str = "MOI/query";
const QUIT_TOPIC: &str = "MOI/quit";
const ALIVE_TOPIC: &str = "MOI/alive";

struct MsgData {
    cfg: SharedPtr<Config>,
    seq: u8,
    m: Mosquitto,
    pending_buffer: Option<Vec<u8>>,
    plugins: Plugins,
}

impl MsgData {
    fn new(cfg: Config, m: &Mosquitto) -> MsgData {
        let cfg = make_shared(cfg);
        MsgData {
            cfg: cfg.clone(),
            seq: 0,
            m: m.clone(),
            pending_buffer: None,
            plugins: Plugins::new(cfg),
        }
    }

    ///// how we package return responses //////
    pub fn ok_result_build(v: JsonValue, addr: String, seq: u8) -> JsonValue {
        if v != JsonValue::Null {
            object! {"id" => addr.as_str(), "seq" => seq, "ok" => v}
        } else {
            v
        }
    }

    pub fn ok_result(&self, v: JsonValue) -> JsonValue {
        MsgData::ok_result_build(v, lock!(self.cfg).addr().into(), self.seq)
    }

    pub fn error_result(&self, msg: &str) -> JsonValue {
        object! {"id" => lock!(self.cfg).addr(), "seq" => self.seq, "error" => msg}
    }

}

// how a remote knows that a query is intended for itself
fn match_condition(cfg: &Config, how: &str, condn: &JsonValue) -> io::Result<bool> {
    if how == "any" || how == "all" {
        let any = how == "any";
        let mut subr = true;
        for item in condn.members() {
            let (subh,subc) = item.entries().next().or_err("all/any conditions need to be arrays")?;
            subr = match_condition(cfg,subh,subc)?;
            if subr {
                if any { return Ok(true) }
            } else {
                if ! any { return Ok(false) }
            }
        }
        return Ok(subr);
    }
    let args = array_of_strings(&condn)?;
    let first_val = if let Ok(val) = cfg.get(args[0]) {
        if how == "exists" { return Ok(true); }
        val
    } else {
        // not an error if a key doesn't exist for any condition
        // not-equal match always succeeds
        return Ok(how=="neq" || how=="nexists");
    };
    (args.len() == 2).or_err("condition needs key and value")?;
    let first_str = first_val.to_string();
    Ok(match how {
        "eq" => first_str == args[1],
        "neq" => first_str != args[1],
        "starts" => { // start pattern
            first_str.starts_with(args[1])
        },
        "elem" => {
            first_val.is_array().or_err("elem only on array values")?;
            first_val.members().any(|v| v == args[1])
        },
        _ => return Err(io_error(&format!("unknown comparison {}",how)).into())
    })
}

fn special_destination_prefix(cfg: &Config, starts: &str) -> PathBuf {
    if cfg.get("destinations").unwrap().contains(starts) {
       cfg.gets(starts).unwrap().into()
    } else {
        starts.into()
    }
}

fn massage_destination_path(cfg: &Config, dest: String) -> PathBuf {
    if dest.starts_with('~') {
        // remote tilde expansion, just like sh
        dest.replace('~',cfg.home()).into()
    } else
    if ! dest.starts_with("/") {
        // Yes this is very Unix-specific but for now that's what remotes are.
        if let Some(slash) = dest.find('/') {
            // the first component of the remote path can be Special
            let starts = &dest[0..slash];
            let rest = &dest[slash+1..];
            special_destination_prefix(cfg,starts).join(rest)
        } else { // single part, should really be a directory
            special_destination_prefix(cfg,&dest)
        }
    } else {
        // absolute remote path, no filtering
        dest.into()
    }
}

fn write_result_code(pcfg: &SharedPtr<Config>, code: i32)  {
   let mut cfg = lock!(pcfg);
   cfg.values.insert("rc".into(),code.into());
}

use std::time::{Duration};

fn cancel_result_code(pcfg: &SharedPtr<Config>)  {
    let cfg = pcfg.clone();
    let timeout = Duration::from_millis(1000);
    thread::spawn(move || {
        thread::sleep(timeout);
        write_result_code(&cfg,0);
    });
}

fn handle_result_code(pcfg: &SharedPtr<Config>, code: i32) {
    if code != 0 {
        write_result_code(pcfg,code);
        cancel_result_code(pcfg);
    }
}

fn handle_verb(mdata: &mut MsgData, verb: &str, args: &JsonValue) -> BoxResult<JsonValue> {
    if verb == "get" {
        let cfg = lock!(mdata.cfg);
        // get a list of keys
        let mut res = JsonValue::new_array();
        for s in args.members() {
            let s = as_str(s)?; // keys must be strings...
            if let Some(val) = mdata.plugins.var(s) { // they may be Special
                res.push(val)?;
            } else {
                // but we return Null if not-found
                res.push(cfg.get_or(s,JsonValue::Null).clone())?;
            }
        }
        Ok(res)
    } else
    if verb == "set" {
        let mut cfg = lock!(mdata.cfg);
        // set keys on this device
        for (key,val) in args.entries() {
            cfg.insert(key, val);
        }
        // we persist the values immediately...
        cfg.write()?;
        Ok(JsonValue::from(true))
    } else
    if verb == "seta" || verb == "rma" {
        let mut cfg = lock!(mdata.cfg);
        // these both modify array-valued keys - rma removes
        // a value from the array if present
        for (key,val) in args.entries() {
            cfg.insert_array(key, val, verb == "rma")?;
        }
        cfg.write()?;
        Ok(JsonValue::from(true))
    } else
    if verb == "run" || verb == "launch" || verb == "spawn" {
        // global tilde substitution needed for standalone tests PASOP
        let (cmd,pwd) = {
            let cfg = lock!(mdata.cfg);
            let home = cfg.home();
            let cmd = string_field(args,"cmd")?.replace('~',home);
            let pwd = string_field(args,"pwd").unwrap_or(home);
            let pwd = massage_destination_path(&cfg,pwd.into());
            (cmd,pwd)
        };
        // check explicitly here because otherwise run_shell_command panics..
        // TODO case where parent exists - don't join filename to dest
        (pwd.exists() && pwd.is_dir())
            .or_then_err(|| format!("run: dest does not exist {}",pwd.display()))?;
        if verb == "run" {
            // we Wait....
            let (code, stdout, stderr) = run_shell_command(&cmd,Some(&pwd));
            handle_result_code(&mdata.cfg,code);
            Ok(object!{"code" => code, "stdout" => stdout, "stderr" => stderr})
        } else
        if verb == "spawn" {
            // we Let Go
            spawn_shell_command(&cmd,Some(&pwd));
            Ok(JsonValue::from(true))
        } else {
            // We immediately return with ok but send results
            // when they are available (moi currently waits
            // patiently for them)
            // TODO put a timeout on this spawned process...
            let m = mdata.m.clone();
            // DUBIOUS - MOI needs to track an _unsolicited_ response
            // here.
            let seq = mdata.seq + 1;
            let addr = lock!(mdata.cfg).addr().to_string();
            let shared_cfg = mdata.cfg.clone();
            let jobname = string_field(args,"job").unwrap_or("<none>").to_string();
            thread::spawn(move || {
                let (code, stdout, stderr) = run_shell_command(&cmd,Some(&pwd));
                let res = object!{"code" => code, "stdout" => stdout, "stderr" => stderr};
                if jobname == "<none>" {
                    // MOI is waiting for us most patiently...
                    let resp = MsgData::ok_result_build(res,addr,seq);
                    m.publish("MOI/result/process",resp.to_string().as_bytes(),1,false).unwrap();
                } else {
                    // not waiting, so we put the result into the store using jobname
                    let mut cfg = lock!(shared_cfg);
                    cfg.insert(&jobname,&res);
                    cfg.write().unwrap();
                }
                // either way, flag 'rc' if we failed!
                handle_result_code(&shared_cfg,code);
            });
            Ok(JsonValue::from(true))
        }
    } else
    if verb == "cp" {
        // Copying files is a two-step process - we first get told that there
        // is a file, with a destination and maybe new permissions. We can
        // complain at this point, e.g. destination does not exist.
        // If we are happy then we subscribe to a retained topic MOI/file
        // which contains the actual bytes.
        let filename = string_field(args,"filename")?;
        let dest = string_field(args,"dest")?.to_string();
        let dest: PathBuf =  massage_destination_path(&lock!(mdata.cfg),dest);

        let maybe_perms = &args["perms"];
        let maybe_hash = &args["hash"];
        let perms = if ! maybe_perms.is_null() {
            Some(
                maybe_perms.as_u32()
                    .or_then_err(|| format!("perms was not an int {}",maybe_perms))?
            )
        } else {
            None
        };
        let hash = if maybe_hash.is_string() {
            Some(maybe_hash.as_str().unwrap().into())
        } else {
            None
        };
        writeable_directory(&dest)?;
        lock!(mdata.cfg).pending_file = Some(FilePending {
            filename: filename.into(),
            dest: dest.join(filename),
            perms: perms,
            hash: hash,
        });
        //println!("pending file set {:?}",cfg.pending_file);
        Ok(JsonValue::from(true))
    } else
    if verb == "fetch" {
        let source = massage_destination_path(&lock!(mdata.cfg),string_field(args,"source")?.into());
        source.exists().or_then_err(|| format!("remote source {} does not exist",source.display()))?;
        mdata.pending_buffer = Some(read_to_buffer(&source)?);
        Ok(JsonValue::from(true))
    } else
    if verb == "restart" {
        let code = args.as_i32().or_err("process code must be integer")?;
        // kill ourselves a little later so we can respond...
        thread::spawn(move || {
            thread::sleep(time::Duration::from_millis(100));
            std::process::exit(code);
        });
        Ok(JsonValue::from(true))
    } else
    if verb == "chain" {
        let mut res = JsonValue::new_array();
        for q in args.members() {
            let (verb,args) = q.entries().next().or_err("chain expects array of objects")?;
            res.push(handle_verb(mdata,verb,args)?)?;
        }
        Ok(res)
    } else {
        if let Some(res) = mdata.plugins.command(verb,args) {
            Ok(res?)
        } else {
            Err(io_error(&format!("unknown command {}",verb)).into())
        }
    }
}

fn handle_query(mdata: &mut MsgData, txt: &str) -> BoxResult<JsonValue> {
    let query = json::parse(txt)?;
    mdata.seq = query["seq"].as_u8().or_err("bad seq")?;
    if let Some((how,condn)) = query["which"].entries().next() {
        // is this query intended for us?
        let yes = match_condition(&lock!(mdata.cfg),how,condn)?;
        if ! yes { // not for us!
            return Ok(JsonValue::Null);
        }
        info!("condition {} {}",how,condn);
    }
    let (verb,args) = query["what"].entries().next()
        .or_err("query must have 'what'")?;
    info!("query {} {}",verb,args);
    handle_verb(mdata,verb,args)
}

fn handle_file(mdata: &mut MsgData, msg: &MosqMessage) -> io::Result<bool> {
    let res = if let Some(ref file) = lock!(mdata.cfg).pending_file {
        let payload = msg.payload();
        let mut oo = fs::OpenOptions::new();
        oo.create(true).write(true);
        if let Some(perms) = file.perms {
            oo.mode(perms);
        }
        let mut outf = oo.open(&file.dest)?;
        outf.write_all(payload)?;
        if let Some(ref hash) = file.hash {
            let digest = md5::compute(&payload);
            let sd = format!("{:x}",digest);
            (sd == hash.as_str()).or_then_err(|| format!("received hash was {} not {}",sd,hash))?;
        }
        true
    } else {
        false
    };
    lock!(mdata.cfg).pending_file = None; // done!
    Ok(res)
}

fn logging_init(cfg: &toml::Value, def: &str) -> BoxResult<()> {

    let file = gets_or_then(cfg,"log_file",|| def.into())?;
    let mut path = PathBuf::from(file);
    if path.is_dir() {
        path = path.join("moid.log");
    } else
    if path.parent() == Some(Path::new("")) {
        path = Path::new(def).join(path);
    }
    logging::init(
        Some(&path),
        gets_or(cfg,"log_level","info")?,
        false
    )?;
    Ok(())
}

use std::os::unix::fs::DirBuilderExt;

fn run() -> BoxResult<()> {
    let file = std::env::args().nth(1).or_err("provide a config file")?;
    if file == "--version" {
        ip4_address("",true);
        println!("MOI daemon version {}",VERSION);
        return Ok(());
    }
    let toml: toml::Value = read_to_string(&file)?.parse()?;
    let toml_config = toml.get("config").or_err("No [config] section")?;

    let root = unsafe { libc::geteuid() == 0 };
    let var_moid = if root {
        let prefix = gets_or(toml_config,"prefix","/usr/local")?;
        let var = gets_or_then(toml_config,"var",|| {
            let dir = format!("{}/var",prefix);
            if let Err(_) = fs::metadata(&dir) {
                fs::create_dir(&dir).expect(&format!("cannot create var dir: {}",dir));
            }
            dir
        })?;
        gets_or_then(toml_config, "log_file", || {
            let dir = format!("{}/moid",var);
            if let Err(_) = fs::metadata(&dir) { // it's private....
                fs::DirBuilder::new().mode(0o700).create(&dir)
                    .expect(&format!("cannot create moi dir: {}",dir));
            }
            dir
        })?
    } else {
        gets_or_then(toml_config,"home",|| env::var("HOME").expect("no damn HOME"))?
    };

    let json_store = gets_or_then(toml_config,"store",|| format!("{}/store.json",var_moid))?;
    let mut store = Config::new_from_file(&toml_config, &PathBuf::from(json_store))?;

    store.insert_into("moid",VERSION);
    store.insert_into("arch",env::consts::ARCH);
    store.insert_into("rc",0);

    // some values in store must be configurable
    store.insert_into("bin",
        gets_or(toml_config,"bin","/usr/local/bin")?
    );

    let addr = store.addr().to_string();
    store.insert_into("tmp",
        gets_or_then(toml_config,"tmp",|| {
            let tmp = env::temp_dir().join(&format!("MOID-{}",addr));
            if ! tmp.is_dir() {
                fs::create_dir(&tmp).expect("could not create tmp dir")
            }
            tmp.to_str().unwrap().to_string()
        })?
    );

    if ! store.values.contains_key("self") {
        store.insert_into("self",env::current_dir()?.to_str().unwrap());
    }
    if ! store.values.contains_key("destinations") {
        let arr = array!["bin","tmp","home","self"];
        store.insert_into("destinations", arr );
    }

    logging_init(&toml_config, &var_moid)?;

    // VERY important that mosquitto client name is unique, otherwise Mosquitto has kittens
    let mosq_name = format!("MOID-{}",&store.addr());
    let default_cert_dir = PathBuf::from(store.gets("self")?).join("certs");
    let m = mosquitto_setup(&mosq_name,&toml_config,&toml,default_cert_dir)?;

    let query = m.subscribe(QUERY_TOPIC,1)?;
    let query_me = m.subscribe( // for speaking directly to us...
            &format!("{}/{}",QUERY_TOPIC,store.addr()),
        1)?;
    let quit = m.subscribe(QUIT_TOPIC,1)?;

    let default_alive_msg = object!{"addr" => store.addr()}.to_string();
    let mut mc = m.callbacks(MsgData::new(store,&m));

    // keepalive strategy for moid is to publish an "I'm alive!" message occaisionally
    // TODO the payload must be customizable
    let ping_timeout = geti_or(toml_config,"alive_interval",60)? as u64;
    let do_reconnect = gets_or(toml_config,"alive_action","reconnect")? == "reconnect";
    let thread_m = m.clone();
    thread::spawn(move || {
        let mut count = 0;
        loop {
            thread::sleep(Duration::from_secs(ping_timeout));
            if let Err(__) = thread_m.publish(ALIVE_TOPIC,default_alive_msg.as_bytes(),1,false) {
                count += 1;
            }
            if count > 3 { // three strikes and we're out...
               error!("three tries out: reconnecting...");
               if do_reconnect {
                   if let Err(e) = thread_m.reconnect() {
                       error!("three tries out: reconnect failed {}",e);
                       process::exit(1);
                   }
               } else {
                   process::exit(1);
               }
               count = 0;
            }
        }
    });

    mc.on_message(|mdata,msg| {
        // TODO error handling is still a mess
        // TODO RETRYING
        if query.matches(&msg) || query_me.matches(&msg) {
            let res = match handle_query(mdata,msg.text()) {
                Ok(v) =>  {
                    info!("seq {} resp {}",mdata.seq,v);
                    mdata.ok_result(v)
                },
                Err(e) => {
                    error!("seq {} resp {}",mdata.seq,e);
                    mdata.error_result(e.description())
                }
            };
            if res != JsonValue::Null { // only tell mother about queries intended for us
                if let Some(ref _pending_file) = lock!(mdata.cfg).pending_file {
                    let topic = &format!("MOI/file/{}",mdata.seq);
                    info!("pending {} seq {}",topic,mdata.seq);
                    m.subscribe(&topic,1).unwrap();
                }
                // the response to "fetch" is a special snowflake
                // successful response is just the bytes of the file
                let file_fetching = if let Some(ref buffer) = mdata.pending_buffer {
                    let cfg = lock!(mdata.cfg);
                    let topic = format!("MOI/fetch/{}/{}/{}",
                        mdata.seq,
                        cfg.addr(),
                        cfg.name()
                    );
                    info!("{} fetched {} bytes",topic,buffer.len());
                    m.publish(&topic,buffer,1,false).unwrap();
                    true
                } else {
                    // But GENERALLY it is in nice JSON format
                    let payload = res.to_string();
                    if let Err(e) = m.publish("MOI/result/query",payload.as_bytes(),1,false) {
                        // TODO: RECONNECTION!
                        error!("publish response failed {}", e);
                    }
                    false
                };
                if file_fetching {
                    mdata.pending_buffer = None;
                }
            }
        } else
        if msg.topic().starts_with("MOI/file") {
            let topic = msg.topic();
            let idx = topic.rfind('/').unwrap();
            mdata.seq = (&topic[idx+1..]).parse().unwrap();
            let res = match handle_file(mdata,&msg) {
                Ok(t) =>  mdata.ok_result(JsonValue::from(t)),
                Err(e) => mdata.error_result(e.description())
            }.to_string();
            if let Err(e) = m.publish("MOI/result/file",res.as_bytes(),1,false) {
                error!("file response failed {}",e);
            }
            m.unsubscribe(msg.topic()).unwrap();
        }

        if quit.matches(&msg) {
            m.disconnect().unwrap();
        }
    });

    m.loop_until_disconnect(-1)?;
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        // might not have logging up yet....??
        eprintln!("error: {}",e);
        std::process::exit(1);
    }
}

