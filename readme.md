# MOI MQTT Orchestration Interface

`moi` came about from a need to manage private networks of embedded Linux devices.
We were already using MQTT with Mosquitto for data passing, so it made sense
to continue using it as the transport layer for a device management system.

We had been investigating Salt Stack in a similar context, and `moi` is in
some ways a reaction to Salt: small, focussed, assuming that the remote
devices are Linux. We can always lean on a minimal POSIX environment
in the remotes.

## Toy Examples

For demonstration purposes, there's a set of JSON config files in the
`examples` folder, and a script for launching four instances of the
`moi` remote daemon, `moid`. It's assumed that Mosquitto is running
on the local machine:

```
examples$ . devices.sh
```
From another terminal, can now run the command-line interface.
The first time `moi` is run, it will create itself a default
configuration file in TOML format.

```
moi$ moi ls
Creating /home/steve/.moi/config.toml.
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
moi$ moi -f addr=10.10.10.10 ls
10.10.10.10	frodo

moi$ moi -f addr=10.10.10.1# ls
10.10.10.10	frodo
10.10.10.11	bilbo
```
There is a slight delay when executing these commands, because a timeout
is used when collecting all the responses (you can use `--timeout` to
specify a different value in milliseconds.)

It's recommended to immediately create a _group_. "all" is considered
special, but you can use any filter condition to create arbitrary
groups. Once defined, you can filter on a group with `--group`:

```
moi$ moi group all
group all created:
10.10.10.10	frodo
10.10.10.23	pippin
10.10.10.22	merry
10.10.10.11	bilbo

moi$ moi -f addr=10.10.10.1# group baggins
group baggins created:
10.10.10.11	bilbo
10.10.10.10	frodo

moi$ moi -g all ls
10.10.10.23	pippin
10.10.10.22	merry
10.10.10.10	frodo
10.10.10.11	bilbo
```
There is an important difference between `moi -g all ls` and `moi ls` - groups are
persistent. If we were to lose a remote, then `moi` would complain. Can simulate
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
and are running in the same directory:

```
moi$ moi run pwd self
10.10.10.10	frodo	/home/steve/rust/repos/moi/examples
10.10.10.11	bilbo	/home/steve/rust/repos/moi/examples
10.10.10.22	merry	/home/steve/rust/repos/moi/examples
10.10.10.23	pippin	/home/steve/rust/repos/moi/examples
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




