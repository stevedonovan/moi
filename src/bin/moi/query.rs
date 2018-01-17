use json::JsonValue;
use md5;
use std::collections::HashMap;
use std::path::PathBuf;
use std::os::unix::fs::PermissionsExt;
use std::fs;
use std::io;
use std::io::prelude::*;

//use moi::*;

use strutil::split_at_delim;

type StringMap = HashMap<String,String>;

// currently we do not want the command API messing with
// these special snowflakes
const DONT_CLOBBER: &[&str] = &["addr","name","time","groups"];

pub fn j_object_to_map(jo: &JsonValue) -> StringMap {
    jo.entries().map(|(k,v)| (k.to_string(),v.to_string())).collect()
}

pub fn to_jarray(vs: &[String]) -> JsonValue {
    let mut res = JsonValue::new_array();
    for key in vs {
        res.push(s(key)).unwrap();
    }
    res
}

// NOTA BENE - value of "null" is special, moves over as Null
pub fn to_jobject(kvs: &StringMap) -> JsonValue {
    let mut res = JsonValue::new_object();
    for (k,v) in kvs {
        let (k,v) = (s(k),s(v)); // as (&str,&str)
        if v == "null" {
            res[k] = JsonValue::Null
        } else {
            res[k] = v.into();
        }
    }
    res
}

fn as_option(s: &Option<String>) -> JsonValue {
    match *s {
        Some(ref s) => s.as_str().into(),
        None => JsonValue::Null
    }
}

fn s (txt: &str) -> &str { txt }

#[derive(Debug)]
pub struct KeyValue {
    pub key: String,
    pub value: String
}

impl KeyValue {
    fn as_jarray(&self) -> JsonValue {
        array![s(&self.key), s(&self.value)]
    }

    pub fn new(key: &str, value: &str) -> KeyValue {
        KeyValue {
            key: key.into(),
            value: value.into()
        }
    }

    pub fn valid_key(key: &str) -> bool {
        key.chars().all(|c| c.is_alphanumeric() || c == '-') &&
         ! DONT_CLOBBER.contains(&key)
    }
}

#[derive(Debug)]
pub enum Condition {
    Equals(KeyValue),
    NotEquals(KeyValue),
    Starts(KeyValue),
    Exists(String),
    Elem(KeyValue),
    Any(Vec<Condition>),
    All(Vec<Condition>),
    None
}

impl Condition {
    pub fn to_json(&self) -> JsonValue {
        match *self {
            Condition::Equals(ref kv) => object!{"eq"=>kv.as_jarray()},
            Condition::NotEquals(ref kv) => object!{"neq"=>kv.as_jarray()},
            Condition::Starts(ref kv) => object!{"starts"=>kv.as_jarray()},
            Condition::Exists(ref key) => object!{"exists"=>array![key.as_str()]},
            Condition::Elem(ref kv) => object!{"elem"=>kv.as_jarray()},
            Condition::All(ref cc) => object!{"all" => Condition::jmap(cc)},
            Condition::Any(ref cc) => object!{"any" => Condition::jmap(cc)},
            Condition::None => JsonValue::Null
        }
    }

    fn jmap(cc: &[Condition]) -> JsonValue {
        let mut res = JsonValue::new_array();
        for c in cc {
            let j = c.to_json();
            res.push(j).unwrap();
        }
        res
    }

    pub fn from_description(txt: &str) -> Condition {
        if txt.starts_with("any ") || txt.starts_with("all ") {
            let any = txt.starts_with("any ");
            let txt = &txt[4..];
            let condns: Vec<_> = txt.split_whitespace()
                .map(|s| Condition::from_description(s)).collect();
            return if any { Condition::Any(condns) } else { Condition::All(condns) };
        }
        if txt == "none" {
            return Condition::None;
        }
        if let Some((k,v)) = split_at_delim(txt,"=") {
            let mut kv = KeyValue::new(k,v);
            if v.ends_with('#') {
                kv.value = (&kv.value[0..v.len()-1]).into();
                Condition::Starts(kv)
            } else {
                Condition::Equals(kv)
            }
        } else
        if let Some((k,v)) = split_at_delim(txt,":") {
            Condition::Elem(KeyValue::new(k,v))
        } else
        if let Some((k,v)) = split_at_delim(txt,".not.") {
            Condition::NotEquals(KeyValue::new(k,v))
        } else {
            Condition::Exists(txt.into())
        }
    }

    pub fn unique_id(&self) -> Option<(String,bool)> {
        if let Condition::Equals(ref kv) = *self {
            if kv.key == "addr" { // addr=ADDR must be unique
                return Some((kv.value.clone(),true));
            } else
            if kv.key == "name" { // should be unique, do a lookup
                return Some((kv.value.clone(),false));
            }
        }
        None

    }
}

pub struct CopyFile {
    pub path: PathBuf,
    pub filename: String,
    pub bytes: Vec<u8>,
    pub dest: String,
    pub perms: Option<u32>,
    pub hash: Option<String>,
}

use std::fmt;

impl fmt::Debug for CopyFile {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "CopyFile {{ path: {:?}, filename: {:?}, bytes: {}b, dest: {:?}, perms: {:?} }}",
            self.path,self.filename,self.bytes.len(),self.dest,self.perms
        )
    }
}

impl CopyFile {
    pub fn new(file: PathBuf, dest: &str) -> io::Result<CopyFile> {
        let filename = file.file_name().unwrap().to_str().unwrap().to_string();
        let perms = file.metadata()?.permissions().mode();
        Ok(CopyFile {
            filename: filename,
            path: file,
            bytes: Vec::new(),
            dest: dest.into(),
            perms: Some(perms),
            hash: None,
        })
    }

    pub fn read_bytes(&mut self) -> io::Result<()> {
        let mut f = fs::File::open(&self.path)?;
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes)?;
        let digest = md5::compute(&bytes);
        self.hash = Some(format!("{:x}",digest));
        self.bytes = bytes;
        Ok(())
    }

    fn to_json(&self) -> JsonValue {
        object! {
            "filename" => s(&self.filename),
            "dest" => s(&self.dest),
            "perms" => self.perms,
            "hash" => as_option(&self.hash),
        }
    }
}

#[derive(Debug)]
pub struct FetchFile {
    pub source: PathBuf,
    pub local_dest: PathBuf,
}

impl FetchFile {
    fn to_json(&self) -> JsonValue {
        object! {"source" => self.source.to_str().unwrap()}
    }
}

#[derive(Debug)]
pub struct RunCommand {
    cmd: String,
    pwd: Option<String>,
    jobname: Option<String>,
}

impl RunCommand {
    pub fn new(cmd: &str, pwd: Option<String>, jobname: Option<String>) -> RunCommand {
        RunCommand {
            cmd: cmd.into(),
            pwd: pwd,
            jobname: jobname,
        }
    }

    fn to_json(&self) -> JsonValue {
        object! {
            "cmd" => self.cmd.as_str(),
            "pwd" => as_option(&self.pwd),
            "job" => as_option(&self.jobname)
        }
    }
}

use std::time::Instant;

#[derive(Debug)]
pub enum Query {
    Get(Vec<String>,String),
    Group(String,Box<Query>),
    Set(StringMap),
    Seta(StringMap),
    Rma(StringMap),
    Run(RunCommand),
    Launch(RunCommand),
    Spawn(RunCommand),
    Copy(CopyFile),
    Fetch(FetchFile),
    Restart(i32),
    Ping(Instant),
    Chain(Vec<Query>),
    Actions(Vec<Query>),
    Wait,
}

fn pair_map(name: &str, value: &str) -> StringMap {
    let mut map = HashMap::new();
    map.insert(name.to_string(),value.to_string());
    map
}

impl Query {
    pub fn get<T: ToString> (v: Vec<T>, command: &str) -> Query {
        let mut args: Vec<String> = v.into_iter().map(|s| s.to_string()).collect();

        if ! args.iter().any(|s| s=="name") {
            args.insert(0,"name".into());
        }

        if ! args.iter().any(|s| s=="addr") {
            args.insert(0,"addr".into());
        }

        Query::Get(args,command.into())
    }

/*
    pub fn columns(&self) -> Vec<String> {
        match *self {
            Query::Get(ref vars) => {
                vars.into()
            },
            Query(
        }
    }
*/
    pub fn group(name: &str) -> Query {
        Query::Group(
            name.into(),
            Box::new(Query::Chain(vec![
                Query::get(vec!["addr","name"],"group"),
                Query::Seta(pair_map("groups",name))
            ]))
        )
    }

    pub fn rma(name: &str, value: &str) -> Query {
        Query::Rma(pair_map(name,value))
    }

    pub fn is_wait(&self) -> bool {
        match *self {
            Query::Wait => true,
            _ => false
        }
    }

    pub fn to_json(&self) -> JsonValue {
        match *self {
            Query::Get(ref vs,_) => object!{"get" => to_jarray(vs)},
            Query::Ping(_) => object!{"get" => array!["addr","name"]},
            Query::Group(_,ref chain) => chain.to_json(),
            Query::Set(ref kvs) => object!{"set"=>to_jobject(kvs)},
            Query::Seta(ref kvs) => object!{"seta"=>to_jobject(kvs)},
            Query::Rma(ref kvs) => object!{"rma"=>to_jobject(kvs)},
            Query::Run(ref r) => object!{"run" => r.to_json() },
            Query::Launch(ref r) => object!{"launch" => r.to_json()},
            Query::Spawn(ref r) => object!{"spawn" => r.to_json()},
            Query::Copy(ref c) => object!{"cp" => c.to_json()},
            Query::Fetch(ref f) => object!{"fetch" => f.to_json()},
            Query::Restart(code) => object!{"restart" => code},
            Query::Chain(ref vq) => {
                let mut res = JsonValue::new_array();
                for v in vq {
                    res.push(v.to_json()).unwrap();
                }
                object!{"chain" => res}
            },
            Query::Wait => JsonValue::Null,
            Query::Actions(_) => panic!("used Actions directly!")
        }
    }
}

