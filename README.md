# emerge presence

Discord rich presence for emerge

# Usage

## dependencies

- python (should be installed with portage anyways)
- rust

## Installing

first clone the repo

```sh
git clone https://github.com/viandoxdev/emerge-presence
```

then build with cargo:

```sh 
cargo build --release
```

the executable will be in `/wherever/you/cloned/it/target/release/emerge-presence`

## Setup

To use this you'll need to add theses lines to the [emerge bashrc](https://wiki.gentoo.org/wiki/Handbook:AMD64/Portage/Advanced#Using_.2Fetc.2Fportage.2Fbashrc_and_affiliated_files):

```bash 
# put in /etc/portage/bashrc

_discordrpcset() {
	echo -en 'set {
		"state": "'"$1"'",
		"category": "'"$CATEGORY"'",
		"package": "'"$PF"'"
	}'"\0" > /tmp/_discordfifo
}
_discordrpcunset() {
	echo -en "unset\0" > /tmp/_discordfifo
}
_discordrpc() {
	if [ -p "/tmp/_discordfifo" ]; then
		case "$EBUILD_PHASE" in
			"setup")
				_discordrpcset "preparing"
				;;
			"compile")
				_discordrpcset "compiling"
				;;
			"preinst")
				_discordrpcset "installing"
				;;
			"postinst")
				_discordrpcunset
				;;
		esac
	fi
}

# might need to add & 2>&1 >/dev/null
_discordrpc
```

You might also need to disable [fs.protected\_fifos](https://docs.kernel.org/admin-guide/sysctl/fs.html#protected-fifos) (I know i needed to):

To do so add this line to `/etc/sysctl.d/local.conf`

```
fs.protected_fifos=0
```

You can then either reboot or enable it instantly with sysctl

```console
# sysctl fs.protected_fifos=0
```

## Starting

To start the daemon:

```sh
RUST_LOG=trace RUST_LOG_STYLE=always /path/to/emerge-presence/target/release/emerge-presence > /tmp/rpcdiscordlogs 2>&1 &
```

can be shortened if logs don't matter (could be useful for debugging)

```sh
/path/to/emerge-presence/target/release/emerge-presence > /dev/null 2>&1 &
```

Could also probably be made into a service and properlly started on boot, in my case I just put an `exec` in my sway config.

## Background

Short summary of how this works, it checks in a loop for the discord ipc, and connects when it can. It also opens a fifo `/tmp/_discordfifo`. When the emerge hooks are triggered (in the bashrc), they write "commands" to the fifo, which are parsed by emerge-presence, which then updates the presence. Commands are strings followed by a json payload (or none), all commands end with a null terminator.

## Notes

This doesn't handle cancelling well, you might just have a neverending presence, you can reset by writing to the fifo:

```sh 
echo -en 'unset\0' > /tmp/_discordfifo
```

## Troubleshooting

You can look at the code, its pretty simple or just ask me.

## License

None.
