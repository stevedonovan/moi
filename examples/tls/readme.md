These are toy TLS files and configurations for you to play with.

Do NOT take them seriously.

`mosquitto` can be invoked with this configuration with

```
tls$ mosquitto -c mosquitto.conf&
```
(throw in a `-v` if you want more verbose details)

`moid` can then be run in the background, and you can run `moi`:

```
tls$ moid test.toml&
tls$ moi -c config.toml ls
10.20.30.10	TLS1
```
