# Troubleshooting

## Logs

While Moonfire NVR is running, logs will be written to stderr.

   * When running `moonfire-nvr config`, you typically should redirect stderr
     to a text file to avoid poor interaction between the interactive stdout
     output and the logging.
   * When running through systemd, stderr will be redirected to the journal.
     Try `sudo journalctl --unit moonfire-nvr` to view the logs. You also
     likely want to set `MOONFIRE_FORMAT=google-systemd` to format logs as
     expected by systemd.

Logging options are controlled by environmental variables:

   * `MOONFIRE_LOG` controls the log level. Its format is similar to the
     `RUST_LOG` variable used by the
     [env-logger](http://rust-lang-nursery.github.io/log/env_logger/) crate.
     `MOONFIRE_LOG=info` is the default.
     `MOONFIRE_LOG=info,moonfire_nvr=debug` gives more detailed logging of the
     `moonfire_nvr` crate itself.
   * `MOONFIRE_FORMAT` selects the output format. The two options currently
     accepted are `google` (the default, like the Google
     [glog](https://github.com/google/glog) package) and `google-systemd` (a
     variation for better systemd compatibility).

## Problems

### `Error: pts not monotonically increasing; got 26615520 then 26539470`

If your streams cut out with an error message like this one, it might mean
that your camera outputs [B
frames](https://en.wikipedia.org/wiki/Video_compression_picture_types#Bi-directional_predicted_.28B.29_frames.2Fslices_.28macroblocks.29).
If you believe this is the case, file a feature request; Moonfire NVR
currently doesn't support B frames. You may be able to configure your camera
to disable B frames in the meantime.

### `moonfire-nvr config` displays garbage

This happens if your machine is configured to a non-UTF-8 locale, due to
gyscos/Cursive#13. As a workaround, type `export LC_ALL=en_US.UTF-8` prior to
running `moonfire-nvr config`.

### Logging in is very very slow

Ensure you're using a build compiled with the `--release` flag. See
[libpasta/libpasta#9](https://github.com/libpasta/libpasta/issues/9) for more
background.
