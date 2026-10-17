//! `postmortem mcp` command line.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use crate::config::McpConfig;
use crate::server::McpServer;

/// Usage text of `postmortem mcp`.
pub const USAGE: &str = "\
Usage: postmortem mcp --case <case folder or case.pmcase> [options]

Serves the case to an AI assistant over the Model Context Protocol on
stdin/stdout. The case is opened read-only unless --allow-writes is given.

Options:
  --case <path>          The case to serve (required)
  --allow-writes         Enable add_tag, remove_tag and save_search_results
  --image-previews       Enable get_image_preview
  --show-cam-content     Return content of files tagged as Child Abuse Material
  --keyword-lists <xml>  The examiner's keywords.xml for run_keyword_list
  --no-audit             Do not write <case>/Log/mcp-audit.jsonl
  --http                 Serve streamable HTTP on 127.0.0.1 instead of stdio
  --port <n>             HTTP port (default 8743)
  --token-file <path>    Where the HTTP bearer token is written (0600)
  -h, --help             Show this help

Client configuration (e.g. Claude Desktop / Claude Code):
  {\"command\": \"postmortem\", \"args\": [\"mcp\", \"--case\", \"/path/to/case\"]}";

/// Parsed command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliArgs {
    pub case: Option<PathBuf>,
    pub config: McpConfig,
    pub http: bool,
    pub token_file: Option<PathBuf>,
    pub help: bool,
}

/// Parses the arguments after `mcp`.
pub fn parse(args: &[OsString]) -> Result<CliArgs, String> {
    let mut out = CliArgs::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let s = a.to_string_lossy();
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match s.as_ref() {
            "--case" => out.case = Some(PathBuf::from(value("--case")?)),
            "--allow-writes" => out.config.allow_writes = true,
            "--image-previews" => out.config.image_previews = true,
            "--show-cam-content" => out.config.show_cam_content = true,
            "--no-audit" => out.config.audit = false,
            "--keyword-lists" => {
                out.config.keyword_lists = Some(PathBuf::from(value("--keyword-lists")?));
            }
            "--http" => out.http = true,
            "--port" => {
                let v = value("--port")?;
                out.config.port = v
                    .to_string_lossy()
                    .parse()
                    .map_err(|_| format!("invalid port {}", v.to_string_lossy()))?;
            }
            "--token-file" => out.token_file = Some(PathBuf::from(value("--token-file")?)),
            "-h" | "--help" => out.help = true,
            other if !other.starts_with('-') && out.case.is_none() => {
                out.case = Some(PathBuf::from(a));
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    if out.case.is_none() && !out.help {
        return Err("--case is required".into());
    }
    Ok(out)
}

/// Runs `postmortem mcp …` (logs go to stderr; stdout carries MCP).
#[allow(clippy::print_stderr, reason = "command-line diagnostics")]
pub fn run(args: &[OsString]) -> ExitCode {
    let cli = match parse(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("postmortem mcp: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if cli.help {
        eprintln!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let Some(case) = cli.case else {
        return ExitCode::from(2);
    };
    let server = match McpServer::open(&case, cli.config.clone()) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("postmortem mcp: cannot open case {}: {e}", case.display());
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(
        case = %case.display(),
        read_only = server.case().is_read_only(),
        "MCP server ready"
    );
    let result = if cli.http {
        crate::http::run_blocking(server, cli.token_file.as_deref())
    } else {
        crate::stdio::serve_stdio(server).map_err(|e| e.to_string())
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("postmortem mcp: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(v: &[&str]) -> Result<CliArgs, String> {
        parse(&v.iter().map(OsString::from).collect::<Vec<_>>())
    }

    #[test]
    fn parses_options() {
        let c = p(&["--case", "/c", "--allow-writes", "--port", "9000", "--http"]).unwrap();
        assert_eq!(c.case, Some(PathBuf::from("/c")));
        assert!(c.config.allow_writes && c.http);
        assert_eq!(c.config.port, 9000);
        assert!(!c.config.image_previews);
        assert_eq!(p(&["/x"]).unwrap().case, Some(PathBuf::from("/x")));
        assert!(p(&[]).is_err());
        assert!(p(&["--case"]).is_err());
        assert!(p(&["--case", "/c", "--bogus"]).is_err());
        assert!(p(&["--help"]).unwrap().help);
    }
}
