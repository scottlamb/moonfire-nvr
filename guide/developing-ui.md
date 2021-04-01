# Working on UI development <!-- omit in toc -->

* [Getting started](#getting-started)
* [Overriding defaults](#overriding-defaults)
* [A note on `https`](#a-note-on-https)

The UI is presented from a single HTML page (index.html) and any number
of Javascript files, css files, images, etc. These are "packed" together
using [webpack](https://webpack.js.org).

For ongoing development it is possible to have the UI running in a web
browser using "hot loading". This means that as you make changes to source
files, they will be detected, the webpack will be recompiled and generated
and then the browser will be informed to reload things. In combination with
the debugger built into modern browsers this makes for a reasonable process.

For a production build, the same process is followed, except with different
settings. In particular, no hot loading development server will be started
and more effort is expended on packing and minimizing the components of
the application as represented in the various "bundles". Read more about
this in the webpack documentation.

## Getting started

Checkout the branch you want to work on and type

```
$ cd ui
$ npm run start
```

This will pack and prepare a development setup. By default the development
server that serves up the web page(s) will listen on
[http://localhost:3000/](http://localhost:3000/) so you can direct your browser
there. It assumes the Moonfire NVR server is running at
[http://localhost:8080/](http://localhost:8080/) and will proxy API requests
there.

Make any changes to the source code as you desire (look at existing code
for examples and typical style), and the browser will hot-load your changes.
Often times you will make mistakes. Anything from a coding error (for which
you can use the browser's debugger), or compilation breaking Javascript errors.
The latter will often be reported as errors during the webpack assembly
process, but some will show up in the browser console, or both.

## Overriding defaults

The UI is setup with [Create React App](https://create-react-app.dev/).
`npm run start` will honor any of the environment variables described in their
[Advanced Configuration](https://create-react-app.dev/docs/advanced-configuration/),
as well as Moonfire NVR's custom `PROXY_TARGET` variable. Quick reference:

| variable       | description                                                              | default                  |
| :------------- | :----------------------------------------------------------------------- | :----------------------- |
| `PROXY_TARGET` | base URL of the backing Moonfire NVR server (see `ui/src/setupProxy.js`) | `http://localhost:8080/` |
| `PORT`         | port to listen on                                                        | 3000                     |
| `HOST`         | host/IP to listen on (or `0.0.0.0` for all)                              | `0.0.0.0`                |

Thus one could connect to a remote Moonfire NVR by specifying its URL as
follows:

```
$ PROXY_TARGET=https://nvr.example.com/ npm run start
```

This allows you to test a new UI against your stable, production Moonfire NVR
installation with real data.

You can also set environment variables in `.env` files, as described in
[Adding Custom Environment Variables](https://create-react-app.dev/docs/adding-custom-environment-variables/).

## A note on `https`

Commonly production setups require credentials and run over `https`, as
described in [secure.md](secure.md). Furthermore, Moonfire NVR will set the
`secure` attribute on cookies it receives over `https`, so that the browser
will only send them over a `https` connection.

This is great for security and somewhat inconvenient for proxying.
Fundamentally, there are three ways to make it work:

   1. Configure the proxy server with valid credentials to supply on every
      request, without requiring the browser to authenticate.
   2. Configure the proxy server to strip out the `secure` attribute from
      cookie response headers, so the browser will send them to the proxy
      server.
   3. Configure the proxy server with a TLS certificate.
         a. using a self-signed certificate manually added to the browser's
            store.
         b. using a certificate from a "real" Certificate Authority (such as
             letsencrypt).

Currently the configuration only implements method 2. It's easy to configure
but has a couple caveats:

   * if you alternate between proxying to a test Moonfire NVR
     installation and a real one, your browser won't know the difference. It
     will supply whichever credentials were sent to it last.
   * if you connect via a host other than localhost, your browser will have a
     production cookie that it's willing to send to a remote host over a
     non-`https` connection. If you ever load this website using an
     untrustworthy DNS server, your credentials can be compromised.

We might add support for method 3 in the future. It's less convenient to
configure but can avoid these problems.
