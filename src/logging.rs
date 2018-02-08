// setting up logging
use log;
use super::*;
use std::fs;
use std::path::Path;
use std::io::prelude::*;

use std::sync::Mutex;

use log::{Log,Record, Level, Metadata, LevelFilter};

use timec::{strftime,now};

fn now_as_str() -> String {
    strftime("%Y-%m-%d %H:%M:%S",&now()).unwrap()
}

struct MoiLogger {
    out: Option<Mutex<fs::File>>,
    hook: Box<Fn(&Record) + Send + Sync + 'static>,
}

impl MoiLogger {
    fn new <C: Fn(&Record) + Send + Sync + 'static>(path: Option<&Path>, hook: C) -> BoxResult<MoiLogger> {
        let out = if let Some(path) = path {
            if ! path.exists() {
                fs::File::create(path)?;
            }
            Some(Mutex::new(fs::OpenOptions::new().append(true).open(path)?))
        } else {
            None
        };
        Ok(MoiLogger{out: out, hook: Box::new(hook)})
    }
}

impl Log for MoiLogger {

    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            if let Some(ref out) = self.out {
                write!(out.lock().unwrap(),
                    "{} [{}] {}\n", now_as_str(), record.level(), record.args()).expect("can't write to log");
            }
            (self.hook)(record);
        }
    }

    fn flush(&self) {}
}

pub fn init<C: Fn(&Record) + Send + Sync + 'static>(log_file: Option<&Path>, level: &str, hook: C) -> BoxResult<()> {
    let level: LevelFilter = level.parse()?;
    let res = log::set_boxed_logger(Box::new(MoiLogger::new(log_file,hook)?));
    log::set_max_level(level);
    if let Err(e) = res {
        return err_io(&format!("logging: {}",e));
    }
    Ok(())
}
