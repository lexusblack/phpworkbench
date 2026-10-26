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
download --path PATH --to LOCAL [--json]   fetch one remote file
delete --path PATH [--recursive] --allow-delete [--json]   remove a remote entry
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

## download

```
php-deploy download --path REMOTE_PATH --to LOCAL_PATH [--json] [--server NAME]
```

Copies one file from `<remote_root>/<path>` to the local `--to` path, creating
missing parent directories.

- `local`: `fs::copy` from `<remote_root>/<path>`.
- `sftp`: runs `get "<remote>" "<local>"` through `sftp -b -`, using the same
  `BatchMode=yes` and `ConnectTimeout=10` settings as the other commands.

`--to` may be absolute or relative to the current directory, but it must not be an
existing directory. Directories are not downloaded. A missing remote path is an
error and exits nonzero.

`--json` prints:

```json
{"server":"local","path":"index.php","to":"/tmp/remote-download/index.php","bytes":42}
```

## delete

```
php-deploy delete --path REMOTE_PATH [--recursive] --allow-delete [--json] [--server NAME]
```

Removes `<remote_root>/<path>`. Deletion is blocked unless `--allow-delete` is
passed. Removing a directory locally also requires `--recursive`.

- `local`: `fs::remove_file` for files, `fs::remove_dir_all` for directories with
  `--recursive`. Without `--recursive`, a directory is refused.
- `sftp`: checks the entry type over `ssh`, then runs `rm` (file) or `rmdir`
  (empty directory) through `sftp -b -`. Recursive directory deletion is not
  supported over sftp and is rejected with a clear error before connecting.

`--json` prints:

```json
{"server":"local","path":"tmp-delete.txt","deleted":true}
```

## Path safety

`--path` is always relative to `remote_root`:

- Absolute paths (leading `/`) are rejected.
- Any `..` segment is rejected, including paths that would normalize back inside
  the root (for example `a/../b`).
- `.` and repeated slashes are normalized; an empty path is rejected.
- `local` additionally canonicalizes the nearest existing ancestor and verifies it
  stays under the canonical `remote_root`, so symlinks cannot escape the root.
- `--to` must not point at an existing directory; it never replaces a directory.
