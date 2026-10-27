use std::ffi::OsString;

// One registry drives applet dispatch and publication of PATH entries.
macro_rules! commands {
    ($( $name:literal => $implementation:ident ),* $(,)?) => {
        pub(super) const NAMES: &[&str] = &[$($name),*];

        pub(super) fn run(name: &str, args: Vec<OsString>) -> Option<i32> {
            let locale = match name {
                "[" => "test",
                "dir" | "vdir" => "ls",
                name => name,
            };
            if !NAMES.contains(&name) {
                return None;
            }
            if let Err(error) = uucore::locale::setup_localization(locale) {
                eprintln!("{name}: {error}");
                return Some(99);
            }
            Some(match name {
                $($name => $implementation::uumain(args.into_iter()),)*
                _ => unreachable!(),
            })
        }
    };
}

// uutils 0.12.0 portable Tier1 set, shared by Linux, macOS and Windows.
commands! {
    "arch" => uu_arch,
    "b2sum" => uu_b2sum,
    "base32" => uu_base32,
    "base64" => uu_base64,
    "basename" => uu_basename,
    "basenc" => uu_basenc,
    "cat" => uu_cat,
    "cksum" => uu_cksum,
    "comm" => uu_comm,
    "cp" => uu_cp,
    "csplit" => uu_csplit,
    "cut" => uu_cut,
    "date" => uu_date,
    "dd" => uu_dd,
    "df" => uu_df,
    "dir" => uu_dir,
    "dircolors" => uu_dircolors,
    "dirname" => uu_dirname,
    "du" => uu_du,
    "echo" => uu_echo,
    "env" => uu_env,
    "expand" => uu_expand,
    "expr" => uu_expr,
    "factor" => uu_factor,
    "false" => uu_false,
    "fmt" => uu_fmt,
    "fold" => uu_fold,
    "head" => uu_head,
    "hostid" => uu_hostid,
    "hostname" => uu_hostname,
    "join" => uu_join,
    "kill" => uu_kill,
    "link" => uu_link,
    "ln" => uu_ln,
    "ls" => uu_ls,
    "md5sum" => uu_md5sum,
    "mkdir" => uu_mkdir,
    "mktemp" => uu_mktemp,
    "more" => uu_more,
    "mv" => uu_mv,
    "nice" => uu_nice,
    "nl" => uu_nl,
    "nohup" => uu_nohup,
    "nproc" => uu_nproc,
    "numfmt" => uu_numfmt,
    "od" => uu_od,
    "paste" => uu_paste,
    "pathchk" => uu_pathchk,
    "pr" => uu_pr,
    "printenv" => uu_printenv,
    "printf" => uu_printf,
    "ptx" => uu_ptx,
    "pwd" => uu_pwd,
    "readlink" => uu_readlink,
    "realpath" => uu_realpath,
    "rm" => uu_rm,
    "rmdir" => uu_rmdir,
    "seq" => uu_seq,
    "sha1sum" => uu_sha1sum,
    "sha224sum" => uu_sha224sum,
    "sha256sum" => uu_sha256sum,
    "sha384sum" => uu_sha384sum,
    "sha512sum" => uu_sha512sum,
    "shred" => uu_shred,
    "shuf" => uu_shuf,
    "sleep" => uu_sleep,
    "sort" => uu_sort,
    "split" => uu_split,
    "sum" => uu_sum,
    "sync" => uu_sync,
    "tac" => uu_tac,
    "tail" => uu_tail,
    "tee" => uu_tee,
    "test" => uu_test,
    "timeout" => uu_timeout,
    "touch" => uu_touch,
    "tr" => uu_tr,
    "true" => uu_true,
    "truncate" => uu_truncate,
    "tsort" => uu_tsort,
    "tty" => uu_tty,
    "uname" => uu_uname,
    "unexpand" => uu_unexpand,
    "uniq" => uu_uniq,
    "unlink" => uu_unlink,
    "uptime" => uu_uptime,
    "vdir" => uu_vdir,
    "wc" => uu_wc,
    "whoami" => uu_whoami,
    "yes" => uu_yes,
    "[" => uu_test,
}
