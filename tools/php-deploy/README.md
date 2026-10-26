# php-deploy

Deploy a local project to a local directory or a remote host over SFTP.

## Configuration

Create `.zed/deploy.json` in the project root:

```json
{
  "default": "local",
  "servers": [
    {
      "name": "local",
      "protocol": "local",
      "local_root": ".",
      "remote_root": "/tmp/php-deploy-target",
      "exclude": ["vendor/", ".git/", "*.log"]
    },
    {
      "name": "staging",
      "protocol": "sftp",
      "host": "example.com",
      "port": 22,
      "user": "deploy",
      "local_root": ".",
      "remote_root": "/var/www/app",
      "exclude": ["vendor/", ".git/", "*.log"]
    }
  ]
}
```

`local_root` is resolved relative to the project root. `remote_root` is used as given:
a filesystem directory for `local`, a remote absolute path for `sftp`.

## Commands

```
php-deploy [--config PATH] [--server NAME] [--dry-run] <command>

status                    show configured servers and resolved paths
check                     validate config and test connectivity
sync                      upload all non-excluded files
upload --file PATH        upload one file
watch [--interval SECS]   poll local files and upload changes
ls [--path PATH] [--json] list a remote directory
```

## ls

```
php-deploy ls [--path REMOTE_PATH] [--json] [--server NAME]
```

Lists one directory under the selected server's `remote_root`; the default path is
`.` (the remote root itself).

- `local`: reads `<remote_root>/<path>` from disk and reports name, type, size, and
  unix modification time.
- `sftp`: runs `ls -1p -- <remote_root>/<path>` over `ssh` with `BatchMode=yes`,
  `ConnectTimeout=10`, and `-p <port>`. Directories are recognised by a trailing
  `/`. Size and modification time are unknown, so they are reported as `null`.

Entries are sorted with directories first, then by name.

Human output is one entry per line:

```
dir  subdir
file README.md  66  1793050865
```

`--json` prints a single object:

```json
{"server":"local","path":".","entries":[{"name":"subdir","type":"dir","size":96,"modified":1793050865},{"name":"README.md","type":"file","size":66,"modified":1793050865}]}
```

`type` is `dir` or `file`. `size` is bytes and `modified` is unix seconds, or `null`
when the transport cannot report them. Errors are written to stderr and exit
nonzero; SSH checks never wait longer than the 10 second connect timeout.
