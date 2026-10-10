# Bundled willdeep-config

Source: https://github.com/niuwoai/willdeep-config

Pinned commit: `cd6348c` (willdeep-config main), package version `0.4.0-rc1`.

The `willdeep-config/` directory contains the unchanged plugin package from this
commit. First-use Web setup installs this package only when no version of
`willdeep-config` is installed. It never replaces an existing package or grants
approval. Users review its declared permissions in the normal plugin center.

The server requires `/usr/bin/ruby`; hosts without it can use terminal onboarding.
The Web host sets `WD_CONFIG_FILE` and `WD_CONFIG_BACKUP_DIR` for this plugin so
editing and backups use the selected configuration and WillDeep home.
