# Working on UI development

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

    $ yarn start

This will pack and prepare a development setup. By default the development
server that serves up the web page(s) will listen on
[http://localhost:3000](http://localhost:3000) so you can direct your browser
there.

Make any changes to the source code as you desire (look at existing code
for examples and typical style), and the browser will hot-load your changes.
Often times you will make mistakes. Anything from a coding error (for which
you can use the browser's debugger), or compilation breaking Javascript errors.
The latter will often be reported as errors during the webpack assembly
process, but some will show up in the browser console, or both.

## Control and location of settings

Much of the settings needed to put the UI together, run webpack etc. is
located in a series of files that contain webpack configuration. These
files all live in the "webpack" subdirectory. We will not explain all
of them here, as you should rarely need to understand them, let alone
modify them.

What is worth mentioning is that the `package.json` file is configured
to use a different webpack configuration for development vs. production
builds. Both configurations depend on a shared configuration common to
both.

There are also some settings that control aspects of the MoonFire UI
behavior, such as window titles etc. These settings are found in the
`settings-nvr.js` file in the project root directory. They should be
pretty self explanatory.

The webpack configuration for all webpack builds is able to load the
values from `settings-nvr.js` and then, if the file exists, load those
from `settings-nvr-local.js` and use them to add to the configuration,
or replace. You can take advantage of this to add your own configuration
values in the "local" file, which does not get checked in, but is used
to affect development, and production builds.

## Special considerations for API calls

The UI code will make calls to the MoonFire NVR's server API, which is
assumed to run on the same host as the MoonFire server. This makes sense
because that server is also normally the one serving up the UI. For UI 
development, however this is not always convenient, or even useful.

For one, you may be doing this development on a machine other than where
the main MoonFire server is running. That can work, but of course that
machine will not be responding the the API calls. If the UI does not
gracefully handle API failure errors (it should but development on that
is ongoing), it may break your UI code.

Therefore, for practical purposes, you may want the API calls to go to
a different server than where the localhost is. Rather than editing the
`webpack/dev.conf.js` file to make this happen, you should use a different
mechanism. The reason for not modifying this file (unless the change is
needed by all), is that the file is under source control and thus should
not reflect settings that are just for your personal use.

The manner in which you can achieve using a different server for API
calls is by telling the development server to use a "proxy" for
certain urls. All API calls start with "/api", so we'll take advantage
of that. Create a the file `settings-nvr-local.js` right next to the standard
`settings-nvr.js`. The file should look something like this:

    module.exports.settings = {
      devServer: {
        proxy: {
          '/api': 'http://192.168.1.232:8080'
        }
      }
    };

This tells the development server to proxy all urls that it encounters
requests for to the url specified. The way this is done is by taking the
full URI component for the original request, and appending it to the string
configured on the right side of the path to be rewritten above. In a standard
MoonFire install, the server (and thus the API server as well), will be
listening on port 8080 a the IP address where the server lives. So adjust
the above for reach your own MoonFire instance where there is a server
running with some real data behind it.

### Issues with "older" MoonFire builds

You might also have though to change the "/api" string in the source code
to include the IP address of the MoonFire server. This would use the
"right" (desirable) URLs, but the requests will fail due to a violation
of the Cross-Origin Resource Sharing (CORS) protocol. If you really
need to, you can add a configuration option to the MoonFire server
by modifying its "service definition". We will not explain here how.

## Changing development server configuration

You can find our standard configuration for the development server inside
the `webpacks/dev.conf.js` file. Using the technique outlined above you
can change ports, ip addresses etc. One example where this may come in
useful is that you may want to "test" your new API code, running on
machine "A" (from a development server), proxying API requests to machine
"B" (because it has real data), from a browser running on machine "C".

The development server normally is configured to listing on port 3000
on "localhost." (which would correspond to machine "A" in this example).
However, you cannot use "localhost" on another machine to refer to "A".
You may think that using the IP address of "A" works, but it doesn't
because "localhost" lives at an IP address local to machine "A".

To make this work you must tell the development server on host "A" to
listen differently. You need to configure it to listen on IP address
"0.0.0.0", which means "all available interfaces". Once that is in
place you can use the IP address to reach "A" from "C". "A" will then
send API requests on to "B", and present final UI using information
from "A" and "B" to the browser on "C".

Modify the local settings to something like this:

    module.exports.settings = {
      devServer: {
        host: "0.0.0.0",
        proxy: {
          '/api': 'http://192.168.1.232:8080'
        }
      }
    };

