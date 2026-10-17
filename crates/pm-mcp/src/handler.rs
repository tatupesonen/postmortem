//! The `rmcp` server handler: maps MCP requests onto [`McpServer`].
//! Tool calls run on Tokio's blocking pool (they do SQLite and evidence
//! I/O).

use std::sync::Arc;

use base64::Engine as _;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, GetPromptRequestParams,
    GetPromptResponse, GetPromptResult, Implementation, ListPromptsResult, ListToolsResult,
    PaginatedRequestParams, Prompt, PromptArgument, PromptMessage, Role, ServerCapabilities,
    ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::Value;

use crate::prompts::{PROMPTS, render};
use crate::server::{INSTRUCTIONS, McpServer, SERVER_NAME};

/// The MCP-facing wrapper of a [`McpServer`].
#[derive(Debug, Clone)]
pub struct McpHandler {
    server: Arc<McpServer>,
}

impl McpHandler {
    pub fn new(server: Arc<McpServer>) -> Self {
        Self { server }
    }

    pub fn server(&self) -> &Arc<McpServer> {
        &self.server
    }

    fn tool_list(&self) -> Vec<Tool> {
        self.server
            .tools()
            .into_iter()
            .map(|t| {
                let schema = match &t.schema {
                    Value::Object(o) => o.clone(),
                    _ => serde_json::Map::new(),
                };
                let mut tool = Tool::new(t.name, t.description.clone(), Arc::new(schema));
                let ann = ToolAnnotations::new()
                    .read_only(!t.write)
                    .destructive(false)
                    .open_world(false);
                tool.annotations = Some(if t.write { ann.idempotent(false) } else { ann });
                tool
            })
            .collect()
    }
}

/// The client's name from the handshake (or the stateless request
/// metadata of protocol 2026-07-28).
fn client_name(ctx: &RequestContext<RoleServer>) -> String {
    if let Some(info) = ctx.peer.peer_info() {
        return info.client_info.name.clone();
    }
    ctx.meta
        .get("io.modelcontextprotocol/clientInfo")
        .and_then(|v| v.get("name"))
        .and_then(Value::as_str)
        .map_or_else(|| "unknown".to_owned(), str::to_owned)
}

impl ServerHandler for McpHandler {
    fn get_info(&self) -> ServerConfig {
        let mut info = Implementation::from_build_env();
        SERVER_NAME.clone_into(&mut info.name);
        env!("CARGO_PKG_VERSION").clone_into(&mut info.version);
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .build(),
        )
        .with_server_info(info)
        .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tool_list()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool_list().into_iter().find(|t| t.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let client = client_name(&context);
        let server = Arc::clone(&self.server);
        let name = request.name.to_string();
        let args = request.arguments.unwrap_or_default();
        let outcome = tokio::task::spawn_blocking(move || server.call(&client, &name, &args))
            .await
            .map_err(|e| ErrorData::internal_error(format!("tool task failed: {e}"), None))?;
        let text = serde_json::to_string(&outcome.json)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let mut content = Vec::with_capacity(2);
        if let Some(png) = &outcome.image {
            content.push(ContentBlock::image(
                base64::engine::general_purpose::STANDARD.encode(png),
                "image/png",
            ));
        }
        content.push(ContentBlock::text(text));
        let result = if outcome.is_error {
            CallToolResult::error(content)
        } else {
            CallToolResult::success(content)
        };
        Ok(result.into())
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        let prompts = PROMPTS
            .iter()
            .map(|p| {
                let args = p
                    .arguments
                    .iter()
                    .map(|(n, d, r)| {
                        PromptArgument::new(*n)
                            .with_description(*d)
                            .with_required(*r)
                    })
                    .collect();
                Prompt::from_raw(p.name, Some(p.description), Some(args))
            })
            .collect();
        Ok(ListPromptsResult::with_all_items(prompts))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        let args = request.arguments.unwrap_or_default();
        let (description, text) =
            render(&request.name, &args).map_err(|e| ErrorData::invalid_params(e, None))?;
        Ok(
            GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)])
                .with_description(description)
                .into(),
        )
    }
}
