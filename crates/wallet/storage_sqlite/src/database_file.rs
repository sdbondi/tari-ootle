// Copyright 2026 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{ffi::OsString, fs, io, path::Path};

/// The files SQLite keeps beside a database in WAL mode, named by appending these suffixes to the database's path: the
/// write-ahead log, and its shared-memory index.
const SIDE_FILE_SUFFIXES: [&str; 2] = ["-wal", "-shm"];

/// Moves the wallet database at `from` to `to`, with the files SQLite keeps beside it. Returns `false` when there is no
/// database at `from`.
///
/// The write-ahead log holds commits not yet copied into the database, so it moves with it: a log left at `from` would
/// be replayed into the next database opened there.
pub fn move_database<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> io::Result<bool> {
    let (from, to) = (from.as_ref(), to.as_ref());
    let moved = rename_if_exists(from.as_os_str().to_owned(), to.as_os_str().to_owned())?;
    for suffix in SIDE_FILE_SUFFIXES {
        rename_if_exists(with_suffix(from, suffix), with_suffix(to, suffix))?;
    }
    Ok(moved)
}

fn with_suffix(path: &Path, suffix: &str) -> OsString {
    let mut path = path.as_os_str().to_owned();
    path.push(suffix);
    path
}

fn rename_if_exists(from: OsString, to: OsString) -> io::Result<bool> {
    match fs::rename(&from, &to) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_write_ahead_log_moves_with_the_database() {
        let temp = tempfile::tempdir().unwrap();
        let from = temp.path().join("wallet.sqlite");
        let to = temp.path().join("wallet.sqlite.bak");
        for file in ["wallet.sqlite", "wallet.sqlite-wal", "wallet.sqlite-shm"] {
            fs::write(temp.path().join(file), file).unwrap();
        }

        assert!(move_database(&from, &to).unwrap());

        for (moved, original) in [
            ("wallet.sqlite.bak", "wallet.sqlite"),
            ("wallet.sqlite.bak-wal", "wallet.sqlite-wal"),
            ("wallet.sqlite.bak-shm", "wallet.sqlite-shm"),
        ] {
            assert!(!temp.path().join(original).exists());
            assert_eq!(fs::read_to_string(temp.path().join(moved)).unwrap(), original);
        }
    }

    #[test]
    fn a_missing_database_is_reported_and_a_stale_log_still_moves() {
        let temp = tempfile::tempdir().unwrap();
        let from = temp.path().join("wallet.sqlite");
        fs::write(temp.path().join("wallet.sqlite-wal"), "").unwrap();

        assert!(!move_database(&from, temp.path().join("wallet.sqlite.bak")).unwrap());
        assert!(!temp.path().join("wallet.sqlite-wal").exists());
    }
}
