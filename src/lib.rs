// Shared code between moi (the cli driver) and moid (the daemon)
// Mostly manages a convenient JSON store
extern crate json;
extern crate toml;
extern crate mosquitto_client;
extern crate get_if_addrs;
#[macro_use] extern crate log;
extern crate time as timec;

pub mod logging;
pub mod toml_utils;
pub mod timeout;
use toml_utils::*;

use std::path::{Path,PathBuf};
use std::io;
use std::io::prelude::*;
use std::fs::File;
use std::time;
use std::process;

use std::collections::HashMap;
use json::JsonValue;
use std::error::Error;

// if it's fine for ripgrep, it's fine for us :)
pub type BoxResult<T> = Result<T,Box<Error>>;

/// Convenience function for making a generic io::Error
// the one constructable error in stdlib
pub fn io_error(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Other, msg)
}

/// Convenience function for making a generic io::Result
pub fn err_io<T>(msg: &str) -> Result<T,Box<Error>> {
    Err(io_error(msg).into())
}

/// This trait maps optional or false values onto `io::Result`
pub trait OrErr<T> {
    /// use when the error message is always a simple string
    fn or_err(self, msg: &str) -> io::Result<T>;

    /// use when the message needs to be constructed
    fn or_then_err<C: FnOnce()->String>(self,fun:C) -> io::Result<T>;
}

impl <T>OrErr<T> for Option<T> {
    fn or_err(self, msg: &str) -> io::Result<T> {
        self.ok_or(io_error(msg))
    }

    fn or_then_err<C: FnOnce()->String>(self,fun:C) -> io::Result<T> {
        self.ok_or_else(|| io_error(&fun()))
    }
}

impl OrErr<()> for bool {
    fn or_err(self, msg: &str) -> io::Result<()> {
        if self {Ok(())} else { Err(io_error(msg)) }
    }

    fn or_then_err<C: FnOnce()->String>(self,fun:C) -> io::Result<()> {
        if self {Ok(())} else { Err(io_error(&fun())) }
    }
}

use std::sync::{Mutex,Arc};
pub type SharedPtr<T> = Arc<Mutex<T>>;

#[macro_export]
macro_rules! lock {
    ($m:expr) => ($m.lock().unwrap())
}

pub fn make_shared<T> (t: T) -> SharedPtr<T> {
    Arc::new(Mutex::new(t))
}

pub trait MoiPlugin {

    fn command(&mut self, _name: &str, _args: &JsonValue) -> Option<BoxResult<JsonValue>> {
        None
    }

    fn var (&self, _name: &str) -> Option<JsonValue> {
        None
    }
}

pub fn current_time_as_secs() -> i64 {
    let now = time::SystemTime::now();
    now.duration_since(time::UNIX_EPOCH).unwrap().as_secs() as i64
}

// you would think that the stdlib would actually provide
// a method to do this...
pub fn duration_as_millis(d: time::Duration) -> f64 {
    1000.0*(d.as_secs() as f64) + (d.subsec_nanos() as f64)/1e6
}

fn lossy_str(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim_right_matches('\n').to_string()
}

// pasop: will blow up if pwd does not exist!
pub fn run_shell_command(cmd: &str, pwd: Option<&Path>) -> (i32,String,String) {
    let mut b = process::Command::new("/bin/sh");
    b.arg("-c").arg(cmd);
    if let Some(pwd) = pwd {
        b.current_dir(pwd);
    }
    let o = b.output().expect("failed to execute shell"); // <--- should we fail here, hm? LOGGING...

    // useful to pass on killed-by-signal info?
    let code = o.status.code().unwrap_or(-1);
    let stdout = lossy_str(&o.stdout);
    let stderr = lossy_str(&o.stderr);
    (code, stdout, stderr)
}

pub fn spawn_shell_command(cmd: &str, pwd: Option<&Path>) -> process::Child {
    let mut b = process::Command::new("/bin/sh");
    b.arg("-c").arg(cmd);
    if let Some(pwd) = pwd {
        b.current_dir(pwd);
    }
    let c = b.spawn().expect("failed to spawn");
    c
}

pub fn ip4_address(interface: &str, noisy: bool) -> Option<String> {
    use get_if_addrs::*;
    let addrs = match get_if_addrs() {
        Ok(addrs) => addrs,
        Err(e) => {
            eprintln!("unable to get network interface {}",e);
            return None;
        }
    };
    for iface in addrs {
        if let IfAddr::V4(ref iface4) = iface.addr {
            let ip = iface4.ip.to_string();
            if noisy {
                println!("interface {} matching {}",ip,interface);
            }
            if iface.name == interface {
                return Some(ip);
            } else
            if interface == "" && ! iface.is_loopback() {
                return Some(ip);
            }
        }

    }
    return None
}

pub fn read_to_string<P: AsRef<Path>>(file: P) -> io::Result<String> {
    let path = file.as_ref();
    let mut f = File::open(path)
        .map_err(|e| io_error(&format!("reading text file {}: {}",path.display(),e)))?;

    let mut s = String::new();
    f.read_to_string(&mut s)?;
    Ok(s)
}

pub fn read_to_buffer<P: AsRef<Path>>(file: P) -> io::Result<Vec<u8>> {
    let path = file.as_ref();
    let mut f = File::open(path)
        .map_err(|e| io_error(&format!("reading binary file {}: {}",path.display(),e)))?;

    let mut buff = Vec::new();
    f.read_to_end(&mut buff)?;
    Ok(buff)
}

pub fn write_all<P: AsRef<Path>>(file: P, contents: &str) -> io::Result<()> {
    let path = file.as_ref();
    let mut f = File::create(path)
        .map_err(|e| io_error(&format!("writing file {}: {}",path.display(),e)))?;

    f.write_all(contents.as_bytes())
}

pub fn writeable_directory<P: AsRef<Path>>(path: P) -> io::Result<()> {
    let dir = path.as_ref();
    (dir.exists() && dir.is_dir() && ! dir.metadata()?.permissions().readonly())
        .or_then_err(|| format!("directory does not exist or is not writeable {}",dir.display()))?;
    Ok(())
}

pub fn as_str(v: &JsonValue) -> io::Result<&str> {
    v.as_str().or_then_err(|| format!("value {} not a string",v))
}

pub fn as_str_always(v: &JsonValue) -> &str {
    as_str(v).unwrap()
}

pub fn array_of_strings(v: &JsonValue) -> io::Result<Vec<&str>> {
    v.members().map(|s| as_str(s)).collect()
}

pub fn strings_to_json<'a, I: Iterator<Item=&'a str>>(v: I) -> JsonValue {
    let mut ja = JsonValue::new_array();
    for s in v {
        ja.push(JsonValue::String(s.into())).unwrap();
    }
    ja
}

pub fn maybe_field<'a>(o: &'a JsonValue, name: &str) -> Option<&'a JsonValue> {
    let val = &o[name];
    if val.is_null() {
        None
    } else {
        Some(val)
    }
}

pub fn field<'a>(o: &'a JsonValue, name: &str) -> io::Result<&'a JsonValue> {
    if let Some(val) = maybe_field(o,name) {
        Ok(val)
    } else {
        Err(io_error(&format!("required field {}",name)))
    }
}

pub fn string_field<'a>(o: &'a JsonValue, name: &str) -> io::Result<&'a str> {
    as_str(field(o,name)?)
}

#[derive(Debug)]
pub struct FilePending {
    pub filename: String,
    pub dest: PathBuf,
    pub perms: Option<u32>,
    pub hash: Option<String>,
}

pub struct Config {
    pub values: HashMap<String,JsonValue>,
    pub file: PathBuf,
    pub pending_file: Option<FilePending>,
}

use std::env;

impl Config {

    pub fn new_from_file(cfg: &toml::Value, file: &Path) -> BoxResult<Config> {

        // initially the store may not exist - this is fine.
        if ! file.exists() {
            write_all(file,"{}\n")?;
        }
        let s = read_to_string(file)?;
        let doc = json::parse(&s)
            .map_err(|e| io_error(&format!("json: {}",e)))?;

        let mut map = HashMap::new();
        for (k,v) in doc.entries() {
            map.insert(k.to_string(),v.clone());
        }

        let mut config = Config{
            values: map,
            file: file.into(),
            pending_file: None,
        };

        config.insert_into("addr",gets_or_then(cfg,"addr",|| {
            let interface = gets_or(cfg,"interface","").expect("interface must be string");
            let ip4 = ip4_address(interface,false).unwrap_or("127.0.0.1".into());
            info!("deduced interface {} IP4 {}",interface,ip4);
            ip4
        })?);

        config.insert_into("name",gets_or_then(cfg,"name",|| {
            let (_,name,_) = run_shell_command("hostname",None);
            name
        })?);

        config.insert_into("home",gets_or_then(cfg,"home",|| {
            env::var("HOME").expect("$HOME not defined. Set 'home' in config")
        })?);
        Ok(config)
    }

    pub fn insert_into<V: Into<JsonValue>>(&mut self, key: &str, val: V) {
        self.values.insert(key.into(),val.into());
    }

    // setting a key to null clears it....
    pub fn insert(&mut self, key: &str, val: &JsonValue) {
        if val == &JsonValue::Null {
            self.values.remove(key);
        } else {
            self.values.insert(key.into(), val.clone());
        }
    }

    // the idea is NOT to add values if already present in the array
    // Must ask explicitly to remove tho
    pub fn insert_array(&mut self, key: &str, val: &JsonValue, remove: bool) -> io::Result<()> {
        let arr = self.values.entry(key.into())
            .or_insert_with(|| JsonValue::new_array());
        (arr.is_array()).or_then_err(|| format!("{} is not array-valued",key))?;

        let present = arr.members().any(|v| v == val);
        if remove {
            if present {
                let pos = arr.members().position(|v| v == val).unwrap();
                arr.array_remove(pos);
            }
        } else
        if ! present {
            arr.push(val.clone()).unwrap();
        }
        Ok(())
    }

    pub fn write(&self) -> io::Result<()> {
        // can make this faster....
        // should we try to keep JsonValues?
        let mut doc = JsonValue::new_object();
        for (k,v) in self.values.iter() {
            doc[k] = v.clone();
        }
        let mut f = File::create(&self.file)?;
        let jout = json::stringify_pretty(doc,2);
        f.write_all(jout.as_bytes())
    }

    pub fn get(&self,key: &str) -> io::Result<&JsonValue> {
        let mut iter = key.split('.');
        let base = iter.next().unwrap();
        let mut obj = self.values.get(base)
            .or_then_err(|| format!("unknown key '{}'",key))?;
        for field in iter {
            obj = &obj[field]
        }
        Ok(obj)
    }

    pub fn get_or(&self,key: &str, def: JsonValue) -> JsonValue {
        match self.get(key) {
            Ok(j) => j.clone(),
            Err(_) => def
        }
    }

    pub fn geti_or(&self,key: &str, def: i32) -> io::Result<i32> {
        Ok(match self.values.get(key) {
            Some(j) => j.as_i32().or_then_err(|| format!("{} must be a string", key))?,
            None => def
        })
    }

    pub fn gets(&self, key: &str) -> io::Result<&str> {
        as_str(self.get(key)?)
    }

    pub fn gets_opt(&self, key: &str) -> io::Result<Option<&str>> {
        match self.values.get(key) {
            None => Ok(None),
            Some(v) => Ok(Some(as_str(v)?))
        }
    }

    pub fn gets_or<'a>(&'a self, key: &str, def: &'static str) -> &'a str
    {
        self.gets(key).unwrap_or(def)
    }

    // we ALWAYS have these...
    pub fn addr(&self) -> &str {
        self.gets("addr").unwrap()
    }

    pub fn home(&self) -> &str {
        self.gets("home").unwrap()
    }

    pub fn name(&self) -> &str {
        self.gets("name").unwrap()
    }
}

pub fn mosquitto_setup(name: &str, config: &toml::Value, toml: &toml::Value, path_def: PathBuf) -> BoxResult<mosquitto_client::Mosquitto> {
    use toml_utils::*;
    let m = mosquitto_client::Mosquitto::new(name);

    if let Some(tls) = toml.get("tls") {
        let path: PathBuf = if let Some(path) = gets_opt(tls,"path")? {
            path.into()
        } else {
            path_def
        };
        let cafile = path.join(gets(tls,"cafile")?);
        let certfile = path.join(gets(tls,"certfile")?);
        let keyfile = path.join(gets(tls,"keyfile")?);
        let passphrase = gets_opt(tls,"passphrase")?;
        info!("TLS {:?} {:?} {:?} {:?}",cafile,certfile,keyfile,passphrase);
        m.tls_set(cafile,certfile,keyfile,passphrase)?;
    } else
    if let Some(tls_psk) = toml.get("tls_psk") {
        let path: PathBuf = if let Some(path) = gets_opt(tls_psk,"path")? {
            path.into()
        } else {
            path_def
        };
        let psk_keyfile = path.join(gets(tls_psk,"psk_file")?);
        let text = read_to_string(&psk_keyfile)?.trim_right_matches('\n').to_string();
        let ciphers = gets_opt(tls_psk,"ciphers")?;
        let (identity,key) = if let Some(idx) = text.find(':') {
            (&text[0..idx], &text[idx+1..])
        } else {
            return err_io("psk key file is iden:bytes");
        };
        info!("TLS-PSK identity {:?} key {:?} ciphers {:?}",identity,key,ciphers);
        m.tls_psk_set(key,identity,ciphers)?;
    }

    {
        let addr = gets_or(config,"mqtt_addr","127.0.0.1")?;
        let port = geti_or(config,"mqtt_port",1883)? as u32;
        info!("MQTT addr {} port {}",addr,port);
        m.connect_wait(addr,port,geti_or(config,"mqtt_connect_wait",300)? as i32)?;
    }
    Ok(m)
}
