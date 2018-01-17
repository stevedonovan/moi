use lapp;
use moi::*;
use std::{env,mem,fs,process};
use std::path::PathBuf;

const VERSION: &str = "0.1.2";

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
  -n, --name (default none) for either address, name or group
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

pub struct CommandArgs {
    pub command: String,
    pub arguments: Vec<String>,
}

pub struct Flags {
    pub filter_desc: String,
    pub group_name: String,
    pub name_or_group: String,
    pub config_file: PathBuf,
    pub moi_dir: PathBuf,
    pub json_store: PathBuf,
    pub timeout: i32,
    pub verbose: bool,
    pub quiet: bool,
   // format: String,
}

impl Flags {
    pub fn new() -> BoxResult<(Vec<CommandArgs>,Flags)> {
        let args = lapp::parse_args(USAGE);
        if args.get_bool("version") {
            println!("MOI comand-line interface version {}",VERSION);
            process::exit(0);
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
                    mem::swap(&mut this_chunk, &mut tmp);
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
            name_or_group: args.get_string("name"),
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
