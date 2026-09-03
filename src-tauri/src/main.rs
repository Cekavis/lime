#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ipc;

use lime_protocol::{
    Config, ConfigSnapshot, DictionaryEntry, InputHistoryEntry, InputHistoryPage, InputRequest,
    ModelInfo, ModelPreset, Request, Response, ServiceStatus, INPUT_HISTORY_PAGE_SIZE,
};
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[tauri::command]
fn get_config() -> Result<ConfigSnapshot, String> {
    match ipc::call(Request::GetConfig)? {
        Response::Config(value) => Ok(value),
        _ => Err("unexpected get_config response".to_owned()),
    }
}

#[tauri::command]
fn set_config(config: Config) -> Result<ConfigSnapshot, String> {
    match ipc::call(Request::SetConfig(config))? {
        Response::Config(value) => Ok(value),
        _ => Err("unexpected set_config response".to_owned()),
    }
}

#[tauri::command]
fn get_status() -> Result<ServiceStatus, String> {
    match ipc::call(Request::GetStatus)? {
        Response::Status(value) => Ok(value),
        _ => Err("unexpected get_status response".to_owned()),
    }
}

#[tauri::command]
fn load_model(path: String) -> Result<ModelInfo, String> {
    match ipc::call(Request::LoadModel { path })? {
        Response::Accepted => match get_status() {
            Ok(status) => Ok(status.model),
            Err(error) => Err(error),
        },
        _ => Err("unexpected load_model response".to_owned()),
    }
}

#[tauri::command]
fn unload_model() -> Result<ModelInfo, String> {
    match ipc::call(Request::UnloadModel)? {
        Response::Accepted => match get_status() {
            Ok(status) => Ok(status.model),
            Err(error) => Err(error),
        },
        _ => Err("unexpected unload_model response".to_owned()),
    }
}

#[tauri::command]
fn list_model_presets() -> Result<Vec<ModelPreset>, String> {
    match ipc::call(Request::ListModelPresets)? {
        Response::ModelPresets(value) => Ok(value),
        _ => Err("unexpected list_model_presets response".to_owned()),
    }
}

#[tauri::command]
fn save_model_preset(name: String, path: String) -> Result<ModelPreset, String> {
    match ipc::call(Request::SaveModelPreset { name, path })? {
        Response::ModelPreset(value) => Ok(value),
        _ => Err("unexpected save_model_preset response".to_owned()),
    }
}

#[tauri::command]
fn delete_model_preset(name: String) -> Result<(), String> {
    match ipc::call(Request::DeleteModelPreset { name })? {
        Response::Accepted => Ok(()),
        _ => Err("unexpected delete_model_preset response".to_owned()),
    }
}

#[tauri::command]
fn select_model_preset(name: String) -> Result<ModelPreset, String> {
    match ipc::call(Request::SelectModelPreset { name })? {
        Response::ModelPreset(value) => Ok(value),
        _ => Err("unexpected select_model_preset response".to_owned()),
    }
}

#[tauri::command]
fn export_dictionary() -> Result<Vec<DictionaryEntry>, String> {
    match ipc::call(Request::ExportDictionary)? {
        Response::Dictionary(value) => Ok(value),
        _ => Err("unexpected export_dictionary response".to_owned()),
    }
}

#[tauri::command]
fn import_dictionary(entries: Vec<DictionaryEntry>) -> Result<(), String> {
    match ipc::call(Request::ImportDictionary { entries })? {
        Response::Accepted => Ok(()),
        _ => Err("unexpected import_dictionary response".to_owned()),
    }
}

#[tauri::command]
fn clear_dictionary() -> Result<(), String> {
    match ipc::call(Request::ClearDictionary)? {
        Response::Accepted => Ok(()),
        _ => Err("unexpected clear_dictionary response".to_owned()),
    }
}

#[tauri::command]
fn test_input(preceding_text: String, preedit: String) -> Result<lime_protocol::InputResponse, String> {
    let config = match ipc::call(Request::GetConfig)? {
        Response::Config(value) => value,
        _ => return Err("unexpected get_config response".to_owned()),
    };
    match ipc::call(Request::Input(InputRequest {
        request_id: TEST_REQUEST_ID.fetch_add(1, Ordering::Relaxed),
        context_available: !preceding_text.is_empty(),
        preceding_text,
        preedit,
        config_revision: config.revision,
    }))? {
        Response::Input(value) => Ok(value),
        _ => Err("unexpected test_input response".to_owned()),
    }
}

#[tauri::command]
fn get_input_history() -> Result<Vec<InputHistoryEntry>, String> {
    match ipc::call(Request::GetInputHistory)? {
        Response::InputHistory(value) => Ok(value),
        _ => Err("unexpected get_input_history response".to_owned()),
    }
}

#[tauri::command]
fn get_input_history_page(page: u32, page_size: Option<u32>) -> Result<InputHistoryPage, String> {
    match ipc::call(Request::GetInputHistoryPage {
        page,
        page_size: page_size.unwrap_or(INPUT_HISTORY_PAGE_SIZE),
    })? {
        Response::InputHistoryPage(value) => Ok(value),
        _ => Err("unexpected get_input_history_page response".to_owned()),
    }
}

#[tauri::command]
fn clear_input_history() -> Result<(), String> {
    match ipc::call(Request::ClearInputHistory)? {
        Response::Accepted => Ok(()),
        _ => Err("unexpected clear_input_history response".to_owned()),
    }
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_config,
            set_config,
            get_status,
            load_model,
            unload_model,
            list_model_presets,
            save_model_preset,
            delete_model_preset,
            select_model_preset,
            export_dictionary,
            import_dictionary,
            clear_dictionary,
            test_input,
            get_input_history,
            get_input_history_page,
            clear_input_history,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Lime management window");
}
