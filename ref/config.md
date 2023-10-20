# Moonfire NVR Configuration File

Moonfire NVR has a small runtime configuration file. By default it's called
`/etc/moonfire-nvr.toml`. You can specify a different path on the commandline,
e.g. as follows:

```console
$ moonfire-nvr run --config /path/to/config.toml
```

`.toml` refers to [Tom's Obvious Minimal Language](https://toml.io/en/). This
is a line-based config format with `[section]` boundaries and `# comment`
lines, meant to be more easily edited by humans.

## Examples

### Starter config

The following is a starter config which allows connecting and viewing video with no authentication:

```toml
[[binds]]
ipv4 = "0.0.0.0:8080"
allowUnauthenticatedPermissions = { viewVideo = true }

[[binds]]
unix = "/var/lib/moonfire-nvr/sock"
ownUidIsPrivileged = true
```

### Authenticated config

The following is for a more secure setup with authentication and a TLS proxy
server in front, as in [guide/secure.md](../guide/secure.md).

```toml
[[binds]]
ipv4 = "0.0.0.0:8080"
trustForwardHeaders = true

[[binds]]
unix = "/var/lib/moonfire-nvr/sock"
ownUidIsPrivileged = true
```

### `systemd` socket activation

`systemd` socket activation (Linux-only) expects `systemd` to create the sockets
on behalf of Moonfire NVR. This can speed startup of services that depend on them and allow
Moonfire to bind to privileged ports (80 or 443) without root privileges. The latter is
expected to be more useful once
[moonfire-nvr#27](https://github.com/scottlamb/moonfire-nvr/issues/27) is
complete and Moonfire is suitable for direct use as an Internet-facing webserver.

To set this up, you'll need an additional systemd unit file for each socket and
to reference them from `/etc/moonfire-nvr.toml`. Be sure to run `sudo systemctl
daemon-reload` to tell `systemd` to read in the new unit files. Your
`moonfire-nvr.service` file should also `Requires=` each socket file.

#### `/etc/moonfire-nvr.toml`

```toml
[[binds]]
systemd = "moonfire-nvr-tcp.socket"
allowUnauthenticatedPermissions = { viewVideo = true }

[[binds]]
systemd = "moonfire-nvr-unix.socket"
ownUidIsPrivileged = true
```

### `/etc/systemd/system/moonfire-nvr.service`

```ini
[Unit]
Requires=moonfire-nvr-tcp.socket
Requires=moonfire-nvr-unix.socket
# ...rest as before...
```

### `/etc/systemd/system/moonfire-nvr-tcp.socket`

```ini
[Socket]
ListenStream=80
Service=moonfire-nvr.service
```

### `/etc/systemd/system/moonfire-nvr-unix.socket`

```ini
[Socket]
ListenStream=/var/lib/moonfire-nvr/sock
Service=moonfire-nvr.service
```

## Reference

At the top level, before any `[[bind]]` lines, the following
keys are understood:

*   `dbDir`: path to the SQLite database directory. Defaults to `/var/lib/moonfire-nvr/db`.
*   `uiDir`: path to the UI to serve. Defaults to `/usr/local/lib/moonfire-nvr/ui`.
*   `workerThreads`: number of [tokio](https://tokio.rs/) worker threads to
    use. Defaults to the number of CPUs on the system. This normally does not
    need to be changed, but reducing it may slightly lower idle CPU usage.

A useful config will bind at least one socket for clients to connect to. Each
should start with a `[[binds]]` line and specify one of the following:

*   `ipv4`: an IPv4 socket address. `0.0.0.0:8080` would allow connections from outside the machine;
    `127.0.0.1:8080` would allow connections only from the local host.
*   `ipv6`: an IPv6 socket address. `[::0]:8080` would allow connections from outside the machine;
    `[[::1]:8080` would allow connections from only the local host.
*   `unix`: a path in the local filesystem where a UNIX-domain socket can be created. Permissions on the
    enclosing directories control which users are allowed to connect to it. Web browsers typically don't
    support directly connecting to UNIX domain sockets, but other tools do, e.g.:
    *   `curl --unix-socket /var/lib/moonfire-nvr/sock http://nvr/api/` will
        issue a request from the commandline. (The hostname in the URL doesn't
        matter.)
    *   `ssh -L localhost:8080:/var/lib/moonfire-nvr/sock moonfire-nvr@nvr-host`
        will allow a web browser on your local machine to connect to the
        Moonfire NVR instance on `nvr-host` via https://localhost:8080/. If
        `ownUidIsPrivileged` is specified (see below), it will additionally
        have all permissions.
*   `systemd` (Linux-only): a name of a socket passed from `systemd`. See
     [`systemd.socket(5)`](https://www.freedesktop.org/software/systemd/man/latest/systemd.socket.html)
     for more information, or the example above.

Additional options within `[[binds]]`:

*   `ownUidIsPrivileged` (UNIX domain sockets only): boolean. If true, a client
    running as Moonfire NVR's own uid can perform any action without additional
    authentication. Once the configuration UI is complete, this will be a handy
    way to set up the first user accounts.
*   `allowUnauthenticatedPermissions`: dictionary. Clients connecting to this
    bind will have the specified permissions, even without UID or session
    authentication. The supported permissions are as in the [`Permissions`
    section of api.md](api.md#permissions).
*   `trustForwardHeaders`: boolean. Moonfire NVR will look for `X-Real-IP` and
    `X-Forwarded-Proto` headers added by a proxy server to determine the
    client's IP address and protocol (`http` or `https`). See
    [guide/secure.md](../guide/secure.md) for more information. *Note:* when
    using this option, ensure that untrusted clients can't bypass the proxy
    server, or they will be able to disguise their true origin.
