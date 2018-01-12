// MOI remote daemon
#[macro_use] extern crate json;
extern crate mosquitto_client;
extern crate moi;

const VERSION: &str = "0.1.1";

use mosquitto_client::{Mosquitto,MosqMessage};
use json::JsonValue;
use moi::*;

// we don't do Windows for now, sorry
use std::os::unix::fs::OpenOptionsExt;
use std::{io,fs,env};
use std::io::prelude::*;
use std::path::{PathBuf};
use std::thread;
use std::time;

//use std::collections::HashMap;
use std::error::Error;

const QUERY_TOPIC: &str = "MOI/query";
const QUIT_TOPIC: &str = "MOI/quit";

struct MsgData {
    cfg: Config,
    seq: u8,
    m: Mosquitto,
    pending_buffer: Option<Vec<u8>>,
}

impl MsgData {
    fn new(cfg: Config, m: &Mosquitto) -> MsgData {
        MsgData {
            cfg: cfg,
            seq: 0,
            m: m.clone(),
            pending_buffer: None,
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
        MsgData::ok_result_build(v, self.cfg.addr().into(), self.seq)
    }

    pub fn error_result(&self, msg: &str) -> JsonValue {
        object! {"id" => self.cfg.addr(), "seq" => self.seq, "error" => msg}
    }

}

// some built-in keys, evaluated on the fly
// TODO obvious customization point...
fn builtin_var(key: &str) -> Option<JsonValue> {
    if key == "time" {
        Some(current_time_as_secs().into())
    } else {
        None
    }
}

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
    if starts == "self" {
        env::current_dir().unwrap()
    } else
    if starts == "home" {
        cfg.home().into()
    } else
    if starts == "tmp" {
        let tmp = env::temp_dir().join("MOID");
        if ! tmp.exists() {
            // hm, this is a bad possibility...
            if ! tmp.exists() {
                fs::create_dir(&tmp).expect("could not create tmp dir");
            }
        }
        tmp
    } else
    if starts == "bin" {
        cfg.gets("bin").unwrap().into()        
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

fn handle_verb(mdata: &mut MsgData, verb: &str, args: &JsonValue) -> BoxResult<JsonValue> {
    if verb == "get" {
        // get a list of keys
        let mut res = JsonValue::new_array();
        for s in args.members() {
            let s = as_str(s)?; // keys must be strings...
            if let Some(val) = builtin_var(s) { // they may be Special
                res.push(val)?;
            } else {
                // but we return Null if not-found
                res.push(mdata.cfg.get_or(s,JsonValue::Null).clone())?;
            }
        }
        Ok(res)
    } else
    if verb == "set" {
        // set keys on this device
        for (key,val) in args.entries() {
            mdata.cfg.insert(key, val);
        }
        // we persist the values immediately...
        mdata.cfg.write()?;
        Ok(JsonValue::from(true))
    } else
    if verb == "seta" || verb == "rma" {
        // these both modify array-valued keys - rma removes
        // a value from the array if present
        for (key,val) in args.entries() {
            mdata.cfg.insert_array(key, val, verb == "rma")?;
        }
        mdata.cfg.write()?;
        Ok(JsonValue::from(true))
    } else
    if verb == "run" || verb == "launch" || verb == "spawn" {
        // global tilde substitution needed for standalone tests PASOP
        let cmd = string_field(args,"cmd")?.replace('~',&mdata.cfg.home());
        let pwd = string_field(args,"pwd").unwrap_or(mdata.cfg.home());
        let pwd = massage_destination_path(&mdata.cfg,pwd.into());
        // check explicitly here because otherwise run_shell_command panics..
        // TODO case where parent exists - don't join filename to dest
        (pwd.exists() && pwd.is_dir())
            .or_then_err(|| format!("run: dest does not exist {}",pwd.display()))?;
        if verb == "run" {
            // we Wait....
            let (code, stdout, stderr) = run_shell_command(&cmd,Some(&pwd));
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
            let addr = mdata.cfg.addr().to_string();
            let jobname = string_field(args,"job").unwrap_or("<none>").to_string();
            thread::spawn(move || {
                let (code, stdout, stderr) = run_shell_command(&cmd,Some(&pwd));
                let res = object!{"code" => code, "stdout" => stdout, "stderr" => stderr};
                if jobname == "<none>" {
                    // MOI is waiting for us most patiently...
                    let resp = MsgData::ok_result_build(res,addr,seq);
                    m.publish("MOI/result/process",resp.to_string().as_bytes(),1,false).unwrap();
                } else {
                    // WEIRD we need to save the results of the job asynchronously
                    // so send a payload to OURSELF so the mosq msg handler can do the writing
                    let resp = array![jobname.as_str(),res];
                    m.publish(&action_me_topic(&addr),resp.to_string().as_bytes(),1,false).unwrap();
                }
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
        let dest: PathBuf =  massage_destination_path(&mdata.cfg,dest);

        let maybe_perms = &args["perms"];
        let perms = if ! maybe_perms.is_null() {
            Some(
                maybe_perms.as_u32()
                    .or_then_err(|| format!("perms was not an int {}",maybe_perms))?
            )
        } else {
            None
        };
        writeable_directory(&dest)?;
        mdata.cfg.pending_file = Some(FilePending {
            filename: filename.into(),
            dest: dest.join(filename),
            perms: perms
        });
        //println!("pending file set {:?}",cfg.pending_file);
        Ok(JsonValue::from(true))
    } else
    if verb == "fetch" {
        let source = massage_destination_path(&mdata.cfg,string_field(args,"source")?.into());
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
        Err(io_error(&format!("unknown command {}",verb)).into())
    }
}

fn handle_query(mdata: &mut MsgData, txt: &str) -> BoxResult<JsonValue> {
    let query = json::parse(txt)?;
    mdata.seq = query["seq"].as_u8().or_err("bad seq")?;
    if let Some((how,condn)) = query["which"].entries().next() {
        // is this query intended for us?
        let yes = match_condition(&mdata.cfg,how,condn)?;
        if ! yes { // not for us!
            return Ok(JsonValue::Null);
        }
    }
    let (verb,args) = query["what"].entries().next()
        .or_err("query must have 'what'")?;
    handle_verb(mdata,verb,args)
}

fn handle_file(mdata: &mut MsgData, msg: &MosqMessage) -> io::Result<bool> {
    let res = if let Some(ref file) = mdata.cfg.pending_file {
        //println!("copying");
        let payload = msg.payload();
        let mut oo = fs::OpenOptions::new();
        oo.create(true).write(true);
        if let Some(perms) = file.perms {
            oo.mode(perms);
        }
        let mut outf = oo.open(&file.dest)?;
        outf.write_all(payload)?;
        true
    } else {
        false
    };
    mdata.cfg.pending_file = None; // done!
    Ok(res)
}

fn action_me_topic(addr: &str) -> String {
    format!("MOI/pvt/store/{}",addr)
}

fn run() -> BoxResult<()> {
    let file = std::env::args().nth(1).or_err("provide a config file")?;
    if file == "--version" {
        println!("MOI daemon version {}",VERSION);
        return Ok(());
    }
    let mut config = Config::new_from_file(&PathBuf::from(file))?;
    config.values.insert("moid".into(),VERSION.into());
    config.values.insert("arch".into(),env::consts::ARCH.into());
    if ! config.values.contains_key("bin") {
        config.values.insert("bin".into(),"/usr/local/bin".into());
    }    

    // VERY important that mosquitto client name is unique, otherwise Mosquitto has kittens
    let mosq_name = format!("MOID-{}",&config.addr());
    let m = Mosquitto::new(&mosq_name);
    m.connect_wait(
        config.gets_or("mqtt_addr","127.0.0.1"),
        config.geti_or("mqtt_port",1883)? as u32,
        config.geti_or("mqtt_connect_timeout",200)?
    )?;
    let query = m.subscribe(QUERY_TOPIC,1)?;
    let query_me = m.subscribe( // for speaking directly to us...
            &format!("{}/{}",QUERY_TOPIC,config.addr()),
        1)?;
    let action_me = m.subscribe(&action_me_topic(&config.addr()),1)?;
    let quit = m.subscribe(QUIT_TOPIC,1)?;
    let mut mc = m.callbacks(MsgData::new(config,&m));

    mc.on_message(|mdata,msg| {
        // TODO error handling is still a mess
        // TODO LOGGING and RETRYING
        if query.matches(&msg) || query_me.matches(&msg) {
            let res = match handle_query(mdata,msg.text()) {
                Ok(v) =>  mdata.ok_result(v),
                Err(e) => mdata.error_result(e.description())
            };
            if res != JsonValue::Null { // only tell mother about queries intended for us
                if let Some(ref _pending_file) = mdata.cfg.pending_file {
                    let topic = &format!("MOI/file/{}",mdata.seq);
                    //println!("pending {} subscribing {:?}",topic,pending_file);
                    m.subscribe(&topic,1).unwrap();
                }
                // the response to "fetch" is a special snowflake
                // successful response is just the bytes of the file
                let file_fetching = if let Some(ref buffer) = mdata.pending_buffer {
                    let topic = format!("MOI/fetch/{}/{}/{}",
                        mdata.seq,
                        mdata.cfg.addr(),
                        mdata.cfg.name()
                    );
                    m.publish(&topic,buffer,1,false).unwrap();
                    true
                } else {
                    // But GENERALLY it is in nice JSON format
                    let payload = res.to_string();
                    if let Err(e) = m.publish("MOI/result/query",payload.as_bytes(),1,false) {
                        // TODO: LOGGING and RECONNECTION!
                        eprintln!("publish response failed {}",e);
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
                eprintln!("file response failed {}",e);
            }
            m.unsubscribe(msg.topic()).unwrap();
        } else
        if action_me.matches(&msg) {
            // this is a kludge. We send _ourselves_ some data
            // to be written to the store directly
            // These unwraps look bad and are evil
            let json = json::parse(msg.text()).unwrap();
            //println!("got {} -> {}",msg.text(),json);
            let key = json[0].as_str().unwrap();
            let value = &json[1];
            mdata.cfg.insert(key,value);
            mdata.cfg.write().unwrap();
        } else
        if quit.matches(&msg) {
            m.disconnect().unwrap();
        }
    });

    m.loop_until_disconnect(-1)?;
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {}",e);
        std::process::exit(1);
    }
}

