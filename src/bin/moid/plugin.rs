// moid plugin definitions go here....
// Mostly empty space, but for now, add your code (or define a new module)
// and make sure it has a public function returning `Box<MoiPlugin>`
// Add the result to the Plugins::new constructor.
use moi::*;
use json::JsonValue;

struct Builtins;

impl MoiPlugin for Builtins {
    fn var (&self, name: &str) -> Option<JsonValue> {
        if name == "time" {
            Some(current_time_as_secs().into())
        } else {
            None
        }
    }
}

fn builtin_init() -> Box<MoiPlugin> {
    Box::new(Builtins)
}

impl Plugins {
    pub fn new(_cfg: SharedPtr<Config>) -> Plugins {
        Plugins {
            plugins: vec![
                builtin_init(),
            ]
        }
    }
}

pub struct Plugins {
    plugins: Vec<Box<MoiPlugin>>
}

impl Plugins {
    pub fn command(&mut self, name: &str, args: &JsonValue) -> Option<BoxResult<JsonValue>> {
        self.plugins.iter_mut().filter_map(|p| p.command(name,args)).next()
    }

    pub fn var (&self, name: &str) -> Option<JsonValue> {
        self.plugins.iter().filter_map(|p| p.var(name)).next()
    }
}
