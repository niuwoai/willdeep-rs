# Bundled willdeep-favorites

Source: https://github.com/niuwoai/willdeep-favorites

Pinned main commit: `04213d92d245fe2c6aafb5927c6868757449c807`, version `2.3.0`.
The nine runtime package files are copied unchanged; untracked `dist/` archives are not used.

CLI/Web startup installs this embedded package without approval or enablement.
Existing versions and data are retained; installing a newer version requires normal approval.
`willdeep plugin builtin install favorites --enable` explicitly approves and enables it.

Ruby is required at `/usr/bin/ruby`. The host sets `WD_FAVORITES_FILE` to
`$WILLDEEP_HOME/plugin-data/willdeep-favorites/favorites.json` unless the package explicitly
overrides it. macOS App's historical favorites store is not migrated or modified.
