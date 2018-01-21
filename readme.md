# MOI MQTT Orchestration Interface

`moi` came about from a need to manage private networks of embedded Linux devices.
We were already using MQTT with Mosquitto for data passing, so it made sense
to continue using it as the transport layer for a device management system.
MQTT involes a broker where clients can _subscribe_ to topics, and _publish_
values to topics. The actual payload contents can be anything. In the case of `moi`,
we publish queries to a topic which all the remotes are listening to; queries
consist of a _filter_ and a _command_ expressed in JSON. They then respond
with a JSON result.

We had been investigating Salt Stack in a similar context, and `moi` is in
some ways a reaction to Salt: small, focussed, assuming that the remote
devices are Unix-like. We can always lean on a minimal POSIX environment
in the remotes.

## No Server (except for broker) just Client

For demonstration purposes, there's a set of JSON config files in the
`examples` folder, and a script for launching four instances of the
`moi` remote daemon, `moid`. It's assumed that Mosquitto is running
on the local machine with the usual defaults:

```
examples$ . devices.sh
```
From another terminal, can now run the command-line interface.
The first time `moi` is run, it will create itself a default
configuration file in TOML format.

```
moi$ moi ls
Creating /home/steve/.local/moi/config.toml.
Edit mqtt_addr if necessary
10.10.10.10	frodo
10.10.10.22	merry
10.10.10.23	pippin
10.10.10.11	bilbo
```
By default, the `ls` command asks each remote to return their values
for the keys `addr` and `name`.  Both `addr` and `name` can be
specified in the `moid` JSON config, but they are deduced if necessary
from examining network interfaces and running `hostname` respectively.

Two built-in values are `moid` (the version of the daemon) and `time`
which is a Unix time stamp:

```
moi$ moi ls time moid
10.10.10.10	frodo	1515508538	0.1.1
10.10.10.22	merry	1515508538	0.1.1
10.10.10.23	pippin	1515508538	0.1.1
10.10.10.11	bilbo	1515508538	0.1.1
```
The key is `moid` (not a generic `version`) because the convention
is that any installed program has a key with its name, and its version
as the value of that key.

Operations filter on keys. There is equality and "starts with"
(I use `#` instead of `*` for the same reason that MQTT topics use
it - it does not receive special expansion by the shell)

```
moi$ moi --filter addr=10.10.10.10 ls
10.10.10.10	frodo

moi$ moi --filter addr=10.10.10.1# ls
10.10.10.10	frodo
10.10.10.11	bilbo
```
There is a slight delay when executing these commands, because a timeout
is used when collecting all the responses (you can use `--timeout`(`-T`) to
specify a different value in milliseconds.)

It's recommended to immediately create the "all" _group_. "all" is considered
special, because `moi` will use it to map addresses to names when displaying
results.

But you can use any filter condition to create arbitrary
groups. Once defined, you can filter on a group with `--group` (or `-g` for short):

```
moi$ moi group all
group all created:
10.10.10.10	frodo
10.10.10.23	pippin
10.10.10.22	merry
10.10.10.11	bilbo

moi$ moi --filter addr=10.10.10.1# group baggins
group baggins created:
10.10.10.11	bilbo
10.10.10.10	frodo

moi$ moi --group all ls
10.10.10.23	pippin
10.10.10.22	merry
10.10.10.10	frodo
10.10.10.11	bilbo
```
There is an important difference between `moi -g all ls` and `moi ls` - groups are
persistent. If we were to lose a remote, then `moi` would complain that the
remote did not respond.

Can simulate
this with the `restart` command, which stops a remote (it has this name because
when run as a service this will result in it being respawned)

```
moi$ moi -f name=merry restart
moi$ moi -g all ls
10.10.10.10	frodo
10.10.10.23	pippin
10.10.10.11	bilbo
error: 10.10.10.22 merry failed to respond

moi$ moi -q -g all ping
error: 10.10.10.22 merry failed to respond
```
After saying `moid merry.json&` in the other terminal, we're back to happiness.
This group behaviour makes it straightforward to quickly detect any missing children,
especially with `ping` used with `--quiet` - output is only produced if there is
an error.

`time` is a quick way to check if remotes are time-synched with some
server - the difference between local and remote time is printed. Like `ping`,
it's basically a specialized `ls` command - there is always a remote key
`time` with the value of the remote's time as a Unix timestamp.

```
moi$ moi -f name=jessie time
192.168.0.13	jessie	1
```

The basic management operations are _pushing_ new files to a remote, and
_running_ commmands remotely:

```
moi$ moi push Cargo.toml '~'
moi$ moi run 'wc -l Cargo.toml'
10.10.10.10	frodo	13 Cargo.toml
10.10.10.23	pippin	13 Cargo.toml
10.10.10.11	bilbo	13 Cargo.toml
10.10.10.22	merry	13 Cargo.toml
```
`push` is given a local file and a remote destination directory - `~` will be
expanded remotely. We have to escape that tilde because, again, the shell
regards it as Special. The special destination `home` means the same, and
is easier to type. By default commands are run in the home directory, but
a second working-directory argument can be specified. Another special
destination name is `self` - the current working directory of the `moid`
process itself. So this works as expected - our remotes are all fake
and are running in the same directory.

Here is `run`: the command `pwd` is run in the working directory `self`.
(They are just our local fakes, so the output isn't very interesting)

```
moi$ moi run pwd self
10.10.10.10	frodo	/home/steve/rust/repos/moi/examples
10.10.10.11	bilbo	/home/steve/rust/repos/moi/examples
10.10.10.22	merry	/home/steve/rust/repos/moi/examples
10.10.10.23	pippin	/home/steve/rust/repos/moi/examples
```
More elaborate commands are tedious to type, because of shell quoting rules.
So `push-run` first pushes a file and then runs a command (note that
permissions of a pushed file are preserved, and another special destination
`tmp`)

```
scratch$ cat space
df -h / | awk '{getline; print $4}'
scratch$ moi -f name=jessie push-run space tmp './space'
192.168.0.13	jessie	18G
```
`push-run` is a convenient shortcut - `moi` supports multi-staged commands separated
by "::".

```
scratch$ moi -f name=jessie push space tmp :: run ./space tmp
192.168.0.13	jessie	18G
```
The filter (or group) is operational for all stages, and every remote must finish
reporting at the end of a stage (within the timeout).

A common operation on remotes which are not Internet-connected is installing packages:
```
scratch$ moi -T 5000 -f name=jessie push-run tree_1.7.0-3_i386.deb tmp 'dpkg -i tree_1.7.0-3_i386.deb'
192.168.0.13	jessie:
(Reading database ... 22048 files and directories currently installed.)
Preparing to unpack tree_1.7.0-3_i386.deb ...
Unpacking tree (1.7.0-3) over (1.7.0-3) ...
Setting up tree (1.7.0-3) ...
Processing triggers for man-db (2.7.0.2-5) ...

scratch$ moi -f name=jessie run tree
192.168.0.13	jessie:
.
├── hello
├── jessie.json
├── moid
└── tree_1.7.0-3_i386.deb

0 directories, 4 files
```
`dpkg` can take some time, when the package cache is cold, so we have to push
up the timeout. There is another solution:

```
scratch$ alias jessie='moi -f name=jessie'
scratch$ jessie push tree_1.7.0-3_i386.deb tmp :: launch 'dpkg -i tree_1.7.0-3_i386.deb' tmp :: wait
```
`launch` is intended for longer-lasting tasks, and here it's used synchronously - `moi` will wait
only as long as is needed, although a long default timeout (20s) is set for the final `wait`.
`moi` will in fact complain if there's no group filter because otherwise it simply does not know
when things have finished - a single `name` or `addr` query counts as a "group of one" for
these purposes.

Sometimes you simply don't want (or need) to wait. `launch` takes a 3rd optional argument,
which is a _job name_. This is a key which you can use to retrieve results later - subfield
matches are supported by `ls`.

```
moi$ jessie launch 'sleep 5 && echo yay' tmp sleep-job
moi$ # Returns immediately. Now wait a bit!
moi$ jessie ls sleep-job
192.168.0.13	jessie	{"code":0,"stdout":"yay","stderr":""}
moi$ jessie ls sleep-job.code
192.168.0.13	jessie	0
scratch$ # can use in a condition...
scratch$ moi -f 'all name="jessie" sleep-job.code=0' ls
192.168.0.13	jessie

```
`pull` retrieves files from remotes. Here the arguments are the remote
file and the local destination file name. This obviously cannot be the
same for _everyone_, so there are some _percent substitutions_ available.
(note the special dir `home` in the remote file name.)

```
scratch$ moi pull home/Cargo.toml %n-cargo.toml
scratch$  ls *cargo.toml
bilbo-cargo.toml  frodo-cargo.toml  merry-cargo.toml pippin-cargo.toml
```
`%n` is the value of `name`, `%a` is the value of `addr`, and `%t` is a
Unix time stamp.

If the destination is given as a directory, then a default pattern is
used: "%n-%a-{remote-filename}"

## Remotes are Key-Value Stores

An important command is `set` which sets a remote named value. (There is
no `get` because it's spelled `ls`.)

```
scratch$ moi set A=1
scratch$ moi ls A
10.10.10.10	frodo	1
10.10.10.11	bilbo	1
10.10.10.22	merry	1
10.10.10.23	pippin	1
```
The special value `null` erases a key:

```
scratch$ moi -g baggins set A=null
scratch$ moi ls A
10.10.10.22	merry	1
10.10.10.23	pippin	1
10.10.10.10	frodo	null
10.10.10.11	bilbo	null
scratch$ moi -f A ls
10.10.10.23	pippin
10.10.10.22	merry
```
The importance of `set` is that `--filter` works on key-values. In the last case, just
giving the key `A` implies that it's a simple existence check. Can check for specific
values:

```
scratch$ moi -g baggins set A=2
scratch$ moi -f A=2 ls
10.10.10.10	frodo
10.10.10.11	bilbo
scratch$ moi -g baggins set A=3 :: ls A
10.10.10.10	frodo	3
10.10.10.11	bilbo	3
```
Typically, you do not want to force an expensive upgrade on stations that are
already upgraded!  So setting keys for installed programs means that only
remotes which match the condition will receive the installer.

## Command Aliases

We had an example of running a more elaborate remote command, and
simplifying the problem with pushing and executing a shell script.

There is another alternative. If `moi` is given a command `foo`, then it will
look for `foo.toml` in current directory, and then `~/moi/foo.toml`.
The structure of that TOML file is straightforward - you must provide
the command name, and an array of arguments. Can also specify a filter
with either `filter` or `group`.

```
scratch$ cat space.toml
command = "run"
args = ["df -h / | awk '{getline; print $4}'"]
filter = "name=jessie"

scratch$ moi space
192.168.0.13	jessie	18G
scratch$ mv space.toml ~/.moi
scratch$ moi space
192.168.0.13	jessie	18G
```
Alternatively, you can edit `~/.moi/config.toml` and add the following
section - `help` is usually a good idea as well!

```toml
[commands.space]
help="how much room has Jessie?"
command = "run"
args = ["df -h / | awk '{getline; print $4}'"]
filter = "name=jessie"
```
It's a matter of taste and convenience whether it's a standalone alias,
or inside the main config TOML.

Aliases can do argument substitution. The `push-run`
pattern for running a script remotely is powerful but it involves repetitive typing:

```
scratch$ moi -f name=jessie push-run space tmp './space'
```
Any arguments to the custom command can be substituted using usual `$` notation.

```
scratch$ tail -n4 ~/.moi/config.toml
[commands.pushr]
help = "push and run a script"
command = "push-run"
args=["$1","tmp","./$1"]

scratch$ moi -f name=jessie pushr space
192.168.0.13	jessie	18G
```
It is possible to do multistage aliases, which are full-blown recipes:

```toml
# deb.toml
help = "installing Debian package"
stages = 4

[1]
command = "push"
args = ["%1","tmp"]

[2]
command = "launch"
args = ["dpkg -i %1","tmp"]

[3]
command = "wait"
args = []

[4]
command = "set"
args = ["$(1:package)=$(1:version)"]

```
Substitutions in aliases are either `$N`, `$(N)` or `$(N:OP)`.
The last line sets a key (made out of the package name) to a value (the package version);
we define a package name as everything up to the first dash or underscore that is
followed by a digit.

A feature of multistage commands is that commands like `launch` and `run` set the special
`rc` variable - if non-zero, subsequent commands will not run. So in this case we can
be sure that the version is _only_ set if the install command succeeds.

```
scratch$ alias jessie='moi -f name=jessie'
scratch$ jessie deb
error: deb installing Debian package index %1 out of range: (0 arguments given)
scratch$ jessie deb tree_1.7.0-3_i386.deb
192.168.0.13	jessie:
(Reading database ... 22048 files and directories currently installed.)
Preparing to unpack tree_1.7.0-3_i386.deb ...
Unpacking tree (1.7.0-3) over (1.7.0-3) ...
Setting up tree (1.7.0-3) ...
Processing triggers for man-db (2.7.0.2-5) ...

scratch$ jessie ls tree
192.168.0.13	jessie	1.7.0-3_i386.deb
```

So, the use of giving "help" is that the error messages are a bit nicer. (It _would_
be cool to have a `moi` command which gives help for all extension commands available.)

## Running on Devices

Although (unfortunately) dated, this upstart `moid.conf` illustrates an
important point:

```
description "MOI Remote Daemon"

start on net-device-up

respawn

chdir /usr/local/etc

exec ./moid device.json

post-stop script
  if test -e moid-*
  then
    cp moid-* moid
    rm moid-*
  fi
end script

```
The actual directories are not important (feelings on the subject can get
both strong and confused) but note the action after the service has stopped:
it will copy a new file over `moid` if it exists. So it is straightforward
to update `moid` using `moid` itself - just give the new executable a
suitable name.

```
$ moi push moid-0.1.2 self :: restart
```

Here is the systemd equivalent, where the contents of `restart-moid` is
the same as the post-stop script above:

```
[Unit]
Description=MOI Remote Daemon
After=multi-user.target

[Service]
WorkingDirectory=/usr/local/bin
ExecStart=/usr/local/bin/moid /usr/local/etc/store.json
Restart=always
ExecStopPost=/usr/local/etc/restart-moid

[Install]
WantedBy=multi-user.target
```

The ease of updating `moid` as a single executable with no dependencies
makes it a good candidate for _customization_. So the idea is to provide
straightforward documented ways for statically linking extra functionality
into `moid`.

## A Start at Documentation

### Keys and Configuration

These are the keys always available from the remote:

  - `name`  settable, invoke `hostname` otherwise
  - `addr`  settable, look for non-local IP4 addresses otherwise.

     (Can specify `interface` in `moid` JSON config if there are
     multiple interfaces)
  - `home`  settable, look at $HOME otherwise
  - `bin` settable, default `/usr/local/bin`
  - `tmp` settable, default `/tmp/MOID-{addr}`
  - `self` settable, default working dir of `moid`
  - `time` time at the remote as Unix timestamp
  - `arch` processor architecture
  - `moid` version of `moid` running
  - `rc` result of last remote command run
  - `destinations` array of special destinations

Keys may consist of alphanumeric characters, plus underscore and dash.
Periods are not valid!

The first three settable vars are set in the TOML file for both `moi` and `moid`:

```toml
[config]
name = "frodo"
addr = "10.20.30.40"
home = "stations/frodo"
```
`bin`, `tmp` and `self` likewise, but are only meaningful for `moid`.

There is in addition three parameters in the `[config]` section for
setting MQTT parameters:

  - `mqtt_addr` - default 'localhost'
  - `mqtt_port` - default 1883
  - `mqtt_connect_wait` - default 300ms

If TLS is used, there is a `[tls]` section. All files are resolved
relative to `path`:

```toml
[tls]
path = "."
cafile = "server.crt"
certfile = "ca.crt"
keyfile = "ca.key"
passphrase = "frodo"
```

### Filters

Here are the basic filters:

   - `KEY=VALUE` test for (string) equality
   - `KEY=VALUE#` true if first part matches up to #
   - `KEY:VALUE`  true if value is found in the _array-valued_ key `KEY`.
      (So "groups:all" matches all devices which belong to the "all" group)
   - `KEY`  true if the key exists at all
   - `KEY.not.VALUE` inequality test

These may be combined, so "--filter 'all A=1 B=2'" matches if all conditions
are true, whereas "--filter 'any A=1 B=2'" matches if any condition is true.

"--group NAME" counts as a filter, although there is some special sauce
involved. `moi` will stop listening as soon as all members of a group have
replied, and will complain bitterly about members that do not reply within
the specified timeout.  A single-remote filter like "addr=ADDR" or "name=NAME"
counts as a group operation - i.e. it is an error for the remote not to reply.

It also implies a further condition, that the variable `rc` is zero. Any
remote command execution sets `rc`, so `::` acts a little bit like `&&` -
subsequent operations can only happen if the previous command succeeded.

### Special Destinations

Generally it's a good idea to let the remotes have preferences for special
directories like their home, where `moid` lives, the temporary dir and
the desired location for programs.

These work with the file operations and
the remote-command operations. So instead of pushing a file to "/tmp", you
just say "tmp" and let the remote handle the details. Simularly, we have
"home", "bin" and "self" (which is `moid` location).

The remote does do tilde-expansion, but "home" is easier to type than
"'~'" (in quotes because the tilde won't survive local shell expansion otherwise.)

Destinations are remote variables but there is a special array-valued variable
`destinations` which actually determines whether a key is used as a
destination.

So it is possible to set new destinations on a remote by setting a variable
and adding it to the `destinations` array:

```
$ moi -f name=frodo set downloads=/home/steve/Downloads :: seta destinations=downloads
$ moi -f name=frodo ls downloads
10.10.10.10	frodo	/home/steve/Downloads
~/c/rust/repos/moi/examples$ moi -f name=frodo run ls downloads
10.10.10.10	frodo:
0d0b8dbb2c6045659713318c472b72db.pdf
1194212734.pdf
....
```

There is an extended syntax for these destinations, modelled on that of `scp/ssh`.
The remote destination can be `{target}:{dest}`:

```
$ moi push test.txt frodo:home
```

This is entirely equivalent to:

```
$ moi -f name=frodo push test.txt home
```
The 'target' here can be one of three things:
  - an IPv4 address
  - a known name (requires the "all" group to be defined)
  - or a group

`--name {target}` (`-n`) has the same effect as the `{target}:{dest}` notation.

`moi` insists that there shall be only one such target specification on the command line.
