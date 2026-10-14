# postmortem brand

* `postmortem-icon.svg` — app icon: a hard-disk platter with a stitched
  autopsy Y-incision. Rendered PNGs at 16–1024 px.
* `postmortem-wordmark.svg` — icon + "post**mortem**" wordmark + tagline
  "digital forensics, dissected".

Colours: incision coral `#FF3B5C` → `#E5173F`, background `#0A0C12`–`#232838`,
platter steel `#F4F6FA` → `#5E6779`.

Regenerate PNGs after editing an SVG: `cargo run -p pm-ui --example render_brand`.
