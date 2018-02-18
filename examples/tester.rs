extern crate moi;
extern crate lapp;
extern crate ansi_term;
use ansi_term::Colour::{Red,Green,Yellow};
use std::process::Command;

use moi::*;

const USAGE: &str = "
tester - run and test CLI programs
    -s, --sorted sort before comparing
    -n, --no-comment don't require comment before each command
    <infile> (infile)
";

struct Flags {
    sorted: bool,
    has_comment: bool,
}

#[derive(Debug)]
struct Test<'a> {
    comment: &'a str,
    command: &'a str,
    lines: Vec<&'a str>,
}

impl <'a> Test<'a> {
    fn create_tests(text: &'a String, flags: &Flags) -> BoxResult<Vec<Test<'a>>> {
        let pos = text.find('$').or_err("must start with $ prompt")?;
        let prompt = &text[0..pos+1];
        let mut iter = text.split(prompt).skip(1);
        let mut tests = Vec::new();
        let mut maybe_chunk = iter.next();
        while let Some(mut chunk) = maybe_chunk {
            let comment = if flags.has_comment {
                let pos = chunk.find('\n').or_err("comment - no linefeed")?;
                let comment = (&chunk[0..pos]).trim();
                if comment.find('#').is_none() {
                    return err_io(&format!("not a comment: {}",comment));
                }
                let comment = comment.trim_left_matches('#').trim_left();
                chunk = iter.next().or_err("no command after comment")?;
                comment
            } else {
                "".into()
            };
            let pos = chunk.find('\n').or_err("command - no linefeed")?;
            let cmd = (&chunk[0..pos]).trim();
            let chunk = &chunk[pos+1..];
            let output = chunk.trim();
            let mut lines: Vec<_> = output.split('\n').collect();
            if flags.sorted {
                lines.sort();
            }
            tests.push(Test {comment: comment, command: cmd, lines: lines});
            maybe_chunk = iter.next();
        }
        Ok(tests)
    }

    fn run(&self, flags: &Flags) {
        let res = shell(self.command);
        let mut olines: Vec<_> = res.split('\n').collect();
        if flags.sorted {
            olines.sort();
        }
        let comment = if flags.has_comment {
            &self.comment
        } else {
            &self.command
        };
        //assert_eq!(olines, self.lines);
        let p_err = Red.bold();
        let p_warn = Yellow.bold();
        let p_ok = Green.bold();
        if olines != self.lines {
            println!("{} {}",comment,p_err.paint("FAILED"));
            println!("{}:",p_warn.paint("expected"));
            for l in &self.lines { println!("{}",l); }
            println!("{}:",p_warn.paint("got"));
            for l in &olines { println!("{}",l); }
            println!();
        } else {
            println!("{} {}",comment,p_ok.paint("OK"));
        }
    }
}


fn run() -> BoxResult<()> {
    let args = lapp::parse_args(USAGE);
    let flags = Flags{
        sorted: args.get_bool("sorted"),
        has_comment: ! args.get_bool("no-comment"),
    };

    let mut text = String::new();
    args.get_infile("infile").read_to_string(&mut text)?;

    let tests = Test::create_tests(&text,&flags)?;
    //debug!(tests);
    for t in &tests {
        t.run(&flags);
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        let text = format!("error: {}",e);
        eprintln!("{}",Red.bold().paint(text));
    }
}

fn shell(cmd: &str) -> String {
    let o = Command::new(if cfg!(windows) {"cmd.exe"} else {"/bin/sh"})
     .arg(if cfg!(windows) {"/c"} else {"-c"})
     .arg(&format!("{} 2>&1",cmd))
     .output()
     .expect("failed to execute shell");
    String::from_utf8_lossy(&o.stdout).trim_right_matches('\n').to_string()
}

