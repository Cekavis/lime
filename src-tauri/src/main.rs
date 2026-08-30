#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ipc;

use lime_protocol::{Config, ConfigSnapshot, DictionaryEntry, ModelInfo, Request, Response, ServiceStatus};

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

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_config,
            set_config,
            get_status,
            load_model,
            unload_model,
            export_dictionary,
            import_dictionary,
            clear_dictionary,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Lime management window");
}
