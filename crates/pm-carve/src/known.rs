//! PhotoRec's file-type ("fileopt") names, as Autopsy validates them
//! (`PhotoRecCarverFileOptExtensions.KNOWN_EXTENSIONS`, generated from
//! PhotoRec's configuration screen). Extension filters must use these.

/// Every name PhotoRec accepts in its `fileopt` options (333).
#[rustfmt::skip]
pub const KNOWN_EXTENSIONS: &[&str] = &[
    "1cd", "3dm", "7z", "DB", "a", "abr", "acb", "accdb", "ace", "ab", "ado", "afdesign", "ahn",
    "aif", "all", "als", "amd", "amr", "apa", "ape", "apple", "ari", "arj", "asf", "asl", "asm",
    "atd", "au", "axp", "axx", "bac", "bdm", "db", "bim", "bin", "binvox", "bkf", "blend",
    "bmp", "bpg", "bvr", "bz2", "c4d", "cab", "caf", "cam", "catdrawing", "cdt", "che", "chm",
    "class", "comicdoc", "cp_", "cow", "cpi", "crw", "csh", "ctg", "cwk", "d2s", "dad", "dar",
    "dat", "dbf", "dbn", "dcm", "ddf", "dex", "diskimage", "fat", "djv", "dmp", "drw", "doc",
    "dpx", "ds2", "DS_Store", "dsc", "dss", "dst", "dta", "dump", "dv", "dvi", "dvr", "dwg",
    "dxf", "e01", "eCryptfs", "edb", "elf", "emf", "ess", "evt", "evtx", "exe", "exs", "ext",
    "ext", "fat", "fbf", "fbk", "fcp", "fcs", "fdb", "fds", "fh10", "fh5", "sparseimage",
    "fits", "fit", "flac", "flp", "flv", "fm", "fob", "fos", "fp5", "fp7", "freeway", "frm",
    "fs", "fwd", "gam", "gct", "gho", "gi", "gif", "gm*", "gp2", "gp5", "gpg", "gpx", "gsm",
    "gz", "hdf", "hdr", "hds", "hfsp", "hm", "hr9", "http", "ibd", "icc", "icns", "ico", "idx",
    "ifo", "imb", "indd", "info", "iso", "it", "itu", "jks", "jpg", "jsonlz4", "kdb", "kdbx",
    "key", "ldf", "lit", "logic", "lnk", "lso", "luks", "lxo", "lzh", "lzo", "m2ts", "mat",
    "max", "mb", "mcd", "mdb", "mdf", "mfa", "mfg", "mft", "mid", "mig", "mk5", "mkv", "mlv",
    "mobi", "mov/mdat", "mov", "mp3", "mpg", "mpl", "mrw", "msa", "mus", "myo", "MYI", "mxf",
    "nd2", "nds", "nes", "njx", "nk2", "nsf", "oci", "ogg", "one", "orf", "paf", "pap", "par2",
    "pcap", "pcb", "pct", "pcx", "pdb", "pdf", "pds", "pf", "pfx", "dump", "plist", "plr",
    "plt", "png", "pnm", "prc", "prd", "prt", "ps", "psb", "psd", "psf", "psp", "pst", "ptb",
    "ptf", "pyc", "pzf", "pzh", "qbb", "qdf", "qkt", "qxd", "r3d", "ra", "raf", "rar", "raw",
    "rdc", "reg", "res", "rfp", "riff", "rlv", "rm", "rns", "rpm", "rw2", "rx2", "save", "ses",
    "sgcta", "shn", "sib", "sit", "skd", "skp", "snag", "snz", "sp3", "spe", "spf", "sav",
    "sqlite", "sqm", "steuer2014", "stl", "studio", "sit", "swf", "tar", "tax", "tg", "tib",
    "tif", "TiVo", "torrent", "tph", "tpl", "ts", "ttf", "tx?", "txt", "tz", "v2i", "vault",
    "vdj", "vfb", "vdi", "veg", "vib", "vmdk", "vmg", "wallet", "wdp", "wee", "wim", "win",
    "wks", "wld", "wmf", "wnk", "woff", "wpb", "wpd", "wtv", "wv", "x3f", "x3i", "x4a", "xar",
    "xcf", "xfi", "xfs", "xm", "xml", "xsv", "xpt", "xv", "xz", "z2d", "zcode", "zip", "zpr",
];
