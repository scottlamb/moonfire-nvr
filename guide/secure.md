# Securing Moonfire NVR and exposing it to the Internet <!-- omit in toc -->

* [The problem](#the-problem)
* [VPN or port forwarding?](#vpn-or-port-forwarding)
* [Overview](#overview)
* [1. Install a webserver](#1-install-a-webserver)
* [2. Configure a static internal IP](#2-configure-a-static-internal-ip)
* [3. Set up port forwarding](#3-set-up-port-forwarding)
* [4. Configure a public DNS name](#4-configure-a-public-dns-name)
* [5. Install a TLS certificate](#5-install-a-tls-certificate)
* [6. Reconfigure Moonfire NVR](#6-reconfigure-moonfire-nvr)
* [7. Configure the webserver](#7-configure-the-webserver)
* [Verify it works](#verify-it-works)

## The problem

After you've completed the [Downloading, installing, and configuring
NVR guide](install.md), you should have a running system you can use from
within your home, but one that is insecure in a couple ways:

  1. It doesn't use `https` to encrypt connections & authenticate itself to
     you.
  2. It doesn't require you to sign in (with your chosen username and
     password) to authenticate yourself to it.

You'll want to change these points if you expose Moonfire NVR's web interface
to the Internet. Security-minded folks would say you shouldn't even allow
unauthenticated sessions within your local network.

Besides security, the nature of home Internet setups presents challenges in
exposing Moonfire NVR to the Internet:

  1. you likely have a single IPv4 address that all your devices share via NAT.
     (Your ISP may also provide a set of IPv6 addresses; even if they do, you
     likely don't have IPv6 available everywhere you want to connect from.)
     You'll need to set up "port forwarding" on your home router, and there
     are many routers with different interfaces for doing so.
  2. that IPv4 address is likely dynamic, so you'll need to configure "dynamic
     DNS" to get a consistent URL to access Moonfire NVR. Most people do this
     through their router's interface as well.
  3. you may want to share your single IP address's `http` and `https` ports
     with other web interfaces, such as a network-attached storage device.
     This requires setting up a proxy and configuring it with each
     destination.
  4. unlike some commercial providers, Moonfire NVR doesn't have any central
     organization to provide a central high-bandwidth, Internet-accessible
     proxying service.

This guide is therefore more abstract than the previous installation steps,
and may even make assumptions that aren't true for your setup. Improvements
are welcome, but it's not possible to make a single terse, concrete guide that
will work for everyone. If you're not a networking expert, you may need to
consult your home router's manual and other external guides or forums.

## VPN or port forwarding?

This guide describes how to set up Moonfire NVR with port forwarding.

Any security camera forums such as [ipcamtalk](https://ipcamtalk.com/) will
recommend that you use a VPN to connect to your NVR rather than port
forwarding. The backstory is that most NVRs are untrustworthy. They have
low-budget, closed-source software written by companies which at best aren't
security-conscious and at worst allow the Chinese government to use [deliberate
backdoors](https://www.reddit.com/r/bestof/comments/8aqyto/user_explains_how_one_chinese_security_camera/).

A VPN's advantage is that it doesn't allow any incoming traffic to reach the
NVR until after authentication, so it's far more secure when the NVR can't be
trusted to perform proper authentication itself.

Port forwarding's advantage is that, once installed on the server, it's far
more convenient to use. There's no VPN client necessary, just a web browser.

I believe Moonfire NVR authenticates properly. It's also open-source, so it's
practical to verify this yourself given sufficient time and expertise.

If you'd prefer to use a VPN, the [ipcamtalk Cliff
Notes](https://ipcamtalk.com/wiki/ip-cam-talk-cliff-notes/) suggest reading
[Network Security
Primer](https://ipcamtalk.com/threads/network-security-primer.1123/) and/or
[VPN Primer for
Noobs](https://ipcamtalk.com/threads/vpn-primer-for-noobs.14601/).

## Overview

  1. Install a webserver
  2. Configure a static internal IP
  3. Set up port forwarding
  4. Configure a public DNS name
  5. Install a TLS certificate
  6. Reconfigure Moonfire NVR
  7. Configure the webserver
  8. Verify it works

## 1. Install a webserver

Moonfire NVR's builtin webserver doesn't yet support `https` (see [issue
\#27](https://github.com/scottlamb/moonfire-nvr/issues/27)), so you'll need to
proxy through a webserver that does. If Moonfire NVR will be sharing an
`https` port with anything else, you'll need to set up the webserver to proxy
to all of these interfaces as well.

I use [nginx](https://nginx.com/) as the proxy server. Some folks may
prefer [Apache httpd](https://httpd.apache.org/) or some other webserver.
Anything will work. I include snippets of a `nginx` config below, so stick
with that if you're not comfortable adapting it to some other server.

I run the proxying webserver on the same machine as Moonfire NVR itself. You
might want to do something else, but this is the simplest setup that means you
only need to configure one machine with a static internal IP address.

digitalocean has a nice [How to install Nginx on Ubuntu 18.04](https://www.digitalocean.com/community/tutorials/how-to-install-nginx-on-ubuntu-18-04) guide.

## 2. Configure a static internal IP

When you configure port forwarding on your router, you'll most likely have to
specify the destination as an internal IP address. You could look up the
current IP address of the webserver machine, but it might change, and your
setup will break if it does.

The easiest way to ensure your setup keeps working is to use the "static DHCP
lease" option on your home router to give your webserver machine the same
address every time it asks for a new lease.

(Alternatively, you can configure your webserver to use a static IP address
instead of asking for a DHCP lease. Ensure the address you choose is outside
the range assigned by the DHCP server, so that there are no conflicts.)

Reboot the webserver machine now and ensure it uses the IP address you choose on
startup, so you don't have a confusing experience after your next power
failure.

## 3. Set up port forwarding

In your router's setup, go to the "Port Forwarding" section and tell it to
forward TCP requests on the `http` port (80) and the `https` port (443) to
your webserver. The `https` port is necessary for secure access, and the
`http` port is necessary for the Let's Encrypt `http-01` challenge during the
setup process.

Now if you go to your external IP address in a web browser, you should reach
your webserver.

## 4. Configure a public DNS name

Also in your router's setup, look for "Dynamic DNS" or "DDNS". Configure it to
update some DNS name with your home's external IP address. You should then be
able to go to this address in a web browser and reach your webserver again.

(It's possible to instead set up a dynamic DNS client on the Moonfire NVR
machine instead. See [this Ubuntu
guide](https://help.ubuntu.com/community/DynamicDNS). One disadvantage is that
it may be slower to recognize IP address changes, so there may be a longer
period in which the address is incorrect.)

## 5. Install a TLS certificate

I recommend using the [Let's Encrypt](https://letsencrypt.org/) Certificate
Authority to obtain a TLS certificate that will be automatically trusted by
your browser. See [How to secure Nginx with Let's Encrypt on Ubuntu
20.04](https://www.digitalocean.com/community/tutorials/how-to-secure-nginx-with-let-s-encrypt-on-ubuntu-20-04).

## 6. Reconfigure Moonfire NVR

If you follow the recommended setup, your `/etc/moonfire-nvr.toml` will contain
this line:

```toml
allowUnauthenticatedPermissions = { viewVideo = true }
```

Replace it with the following:

```toml
trustForwardHeaders = true
```

This change has two effects:

   * No `allowUnauthenticatedPermissions` means that web users must
     authenticate.
   * `trustForwardHeaders` means that Moonfire NVR will look for `X-Real-IP`
     and `X-Forwarded-Proto` headers as added by the webserver configuration
     in the next section.

See also [ref/config.md](../ref/config.md) for more about the configuration file.

If the webserver is running on the same machine as Moonfire NVR, you might
also change the `ipv4 = "0.0.0.0:8080"` line in `/etc/moonfire-nvr/toml` to
`ipv4 = "127.0.0.1:8080"`, so that only the local host can directly connect to
Moonfire NVR. If other machines can connect directly, they can impersonate
the proxy, which would effectively allow them to lie about the client's IP and
protocol.

To make this take effect, you'll need to restart Moonfire NVR:

```console
$ sudo systemctl restart moonfire-nvr
```

## 7. Configure the webserver

Since step 5, you should have a `https`-capable webserver set up on your
desired DNS name. Now finalize its configuration:

   * redirect all `http` traffic to `https`
   * proxy `https` traffic to Moonfire NVR
   * when proxying, set the `X-Real-IP` header to the original IP address
     (removing any previous occurrences of this header)
   * when proxying, set the `X-Forwarded-Proto` header to the original
     protocol (which should be `https` if you've configured everything
     correctly).

The author's system does this via the following
`/etc/nginx/sites-available/nvr.home.slamb.org` file:

```nginx
upstream moonfire {
    server 127.0.0.1:8080;
}

map $http_upgrade $connection_upgrade {
    default Upgrade;
    ''      close;
}

server {
    root /var/www/html;
    index index.html index.htm index.nginx-debian.html;

    server_name nvr.home.slamb.org;

    location / {
        proxy_pass http://moonfire;
        # try_files $uri $uri/ =404;
    }

    proxy_http_version 1.1;
    proxy_buffering off;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection $connection_upgrade;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header Host $http_host;
    proxy_redirect http:// $scheme://;

    listen [::]:443 ssl ipv6only=on; # managed by Certbot
    listen 443 ssl; # managed by Certbot
    ssl_certificate /etc/letsencrypt/live/nvr.home.slamb.org/fullchain.pem; # managed by Certbot
    ssl_certificate_key /etc/letsencrypt/live/nvr.home.slamb.org/privkey.pem; # managed by Certbot
    include /etc/letsencrypt/options-ssl-nginx.conf; # managed by Certbot
    ssl_dhparam /etc/letsencrypt/ssl-dhparams.pem; # managed by Certbot

}

server {
    listen 80;
    listen [::]:80;

    return 301 https://nvr.home.slamb.org$request_uri;

    server_name nvr.home.slamb.org nvr;
}
```

Check your configuration for syntax errors and reload it:

```
$ sudo nginx -t
$ sudo systemctl reload nginx
```

## Verify it works

Go to `http://your.domain.here/api/request` and verify the following:

   * the browser redirects from `http` to `https`
   * the address shown here matches your web browser's public IP address.
     (Compare to [https://whatsmyip.com/](https://whatsmyip.com/).)
   * the page says `secure: true` indicating you are using `https`.

Then go to `https://your.domain.here/` and you should see the web interface,
including a login form.

Login with the credentials you added through `moonfire-nvr config` in the
[previous guide](install.md). You should see your username and "logout" in the
upper-right corner of the web interface.

Also try the live streaming feature, which requires WebSockets. The nginx
configuration above includes sections derived from nginx's [NGINX as a
WebSocket Proxy](https://www.nginx.com/blog/websocket-nginx/) doc.

If it doesn't work as expected, re-read this guide, then open an issue on
github for help.
