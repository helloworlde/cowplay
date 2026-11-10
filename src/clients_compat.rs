//! Prepare an older-version browser preference snapshot without changing active state.
use std::{fs::{self,OpenOptions},io::Write,os::unix::fs::{OpenOptionsExt,PermissionsExt},path::Path};
use serde_json::Value;

pub fn prepare(state:&Path,output:&Path)->Result<Value,String>{
    let bytes=f50_server::settings::read(state.join("clients.json").to_str().ok_or("状态路径必须为UTF-8")?,1024*1024).map_err(|_|"无法读取客户端状态")?;
    let clients:Vec<f50_server::clients::Client>=serde_json::from_slice(&bytes).map_err(|_|"客户端状态格式无效")?;
    if clients.len()>64||clients.iter().any(|c|c.id.len()!=32||c.prefs.validate().is_err()){return Err("客户端状态未通过校验".into());}
    let mut value:Value=serde_json::from_slice(&bytes).map_err(|_|"客户端状态格式无效")?;
    let mut sizes=0;
    for client in value.as_array_mut().ok_or("客户端列表无效")? {
        let prefs=client.get_mut("prefs").and_then(Value::as_object_mut).ok_or("客户端偏好无效")?;
        prefs.remove("right_hand_drive");
        if prefs.get("resolution").and_then(Value::as_str).is_some_and(|r|!matches!(r,"native"|"1.5x"|"720p"|"1080p"|"1440p"|"2160p")) {
            prefs.insert("resolution".into(),Value::String("native".into()));sizes+=1;
        }
    }
    let parent=output.parent().ok_or("需要私有输出目录")?;
    let metadata=fs::symlink_metadata(parent).map_err(|_|"输出目录不存在")?;
    if !metadata.is_dir()||metadata.file_type().is_symlink()||metadata.permissions().mode()&0o077!=0{return Err("输出目录必须为现有的0700私有目录".into());}
    let mut file=OpenOptions::new().write(true).create_new(true).mode(0o600).open(output).map_err(|_|"输出文件已存在或不可创建")?;
    let data=serde_json::to_vec_pretty(&value).map_err(|_|"无法编码兼容状态")?;
    if file.write_all(&data).and_then(|_|file.sync_all()).is_err(){drop(file);let _=fs::remove_file(output);return Err("无法完整写入兼容状态".into());}
    Ok(serde_json::json!({"clients":clients.len(),"exact_sizes_reset":sizes,"active_state_changed":false}))
}

#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn downgrade_copy_preserves_authorization_and_never_replaces_current_or_existing_files(){
        let dir=tempfile::tempdir().unwrap();fs::set_permissions(dir.path(),fs::Permissions::from_mode(0o700)).unwrap();
        let store=f50_server::clients::Clients::new("123456");store.load(dir.path().to_owned()).unwrap();
        let client=store.admit_token("123456","screen","synthetic","","").unwrap();
        let mut prefs=client.prefs.clone();prefs.quality="custom".into();prefs.resolution="720x1280".into();prefs.right_hand_drive=true;prefs.codec="hevc".into();store.set_prefs(&client.id,prefs).unwrap();
        let original=fs::read(dir.path().join("clients.json")).unwrap();let output=dir.path().join("clients-compatible.json");
        assert_eq!(prepare(dir.path(),&output).unwrap()["exact_sizes_reset"],1);
        assert_eq!(fs::read(dir.path().join("clients.json")).unwrap(),original);
        let mut expected:Value=serde_json::from_slice(&original).unwrap();let p=expected[0]["prefs"].as_object_mut().unwrap();p.remove("right_hand_drive");p.insert("resolution".into(),Value::String("native".into()));
        let converted:Value=serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();assert_eq!(converted,expected);
        let p=converted[0]["prefs"].as_object().unwrap();assert!(p.keys().all(|k|["quality","resolution","fps","codec","call","rail_side","ui_size","night","debug"].contains(&k.as_str())),"only the old preference schema remains");
        assert_eq!(fs::metadata(&output).unwrap().permissions().mode()&0o777,0o600);
        assert!(prepare(dir.path(),&output).is_err());assert_eq!(fs::read(dir.path().join("clients.json")).unwrap(),original);
        let public=dir.path().join("public");fs::create_dir(&public).unwrap();fs::set_permissions(&public,fs::Permissions::from_mode(0o755)).unwrap();assert!(prepare(dir.path(),&public.join("copy")).is_err());
    }
}
