# Linux desktop integration

`dev.pilcrow.Pilcrow.desktop` matches the window `app_id` used by
`WindowOptions`. Install it to `/usr/share/applications` (or
`~/.local/share/applications`) together with a `dev.pilcrow.Pilcrow` icon, and
ensure `pilcrow` is on `PATH`.

The desktop entry claims `text/markdown` and passes selected files as argv;
the app's launch path accepts the first `.md`/`.markdown` argument.
