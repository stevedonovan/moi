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
}

impl MoiLogger {
    fn new (path: Option<&Path>) -> BoxResult<MoiLogger> {
        let out = if let Some(path) = path {
            Some(Mutex::new(fs::File::create(path)?))
        } else {
            None
        };

        Ok(MoiLogger{out: out})
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
            if record.level() == Level::Error {
                eprintln!("{}",record.args());
            }
        }
    }

    fn flush(&self) {}
}

pub fn init(log_file: Option<&Path>, level: &str) -> BoxResult<()> {
    let level: LevelFilter = level.parse()?;
    let res = log::set_boxed_logger(Box::new(MoiLogger::new(log_file)?));
    log::set_max_level(level);
    if let Err(e) = res {
        return err_io(&format!("logging: {}",e));
    }
    Ok(())
}
