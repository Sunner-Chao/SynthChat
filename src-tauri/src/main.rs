// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Some(action) = synthchat_v1_lib::acp_cli_action_from_args(std::env::args()) {
        let result = match action {
            synthchat_v1_lib::AcpCliAction::Stdio => synthchat_v1_lib::run_acp_stdio(),
            synthchat_v1_lib::AcpCliAction::McpStdio => synthchat_v1_lib::run_mcp_stdio(),
            synthchat_v1_lib::AcpCliAction::Version => {
                synthchat_v1_lib::print_acp_version();
                Ok(())
            }
            synthchat_v1_lib::AcpCliAction::Check => synthchat_v1_lib::run_acp_check(),
            synthchat_v1_lib::AcpCliAction::Setup => synthchat_v1_lib::run_acp_setup(),
            synthchat_v1_lib::AcpCliAction::SetupBrowser => {
                synthchat_v1_lib::run_acp_setup_browser()
            }
        };
        if let Err(error) = result {
            eprintln!("SynthChat ACP command failed: {error}");
            std::process::exit(1);
        }
        return;
    }
    synthchat_v1_lib::run()
}
