use super::Admin;
use crate::tesla_ble::management::Command;
use serde_json::Value;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Configure {
    vin: Option<String>,
    enabled: bool,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UltraControl { enabled: bool, session: u64, context: u64 }
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ClimateRequest {
    action: String,
    on: Option<bool>,
    driver_celsius: Option<f32>,
    passenger_celsius: Option<f32>,
}

fn status(a: &Admin, status: crate::tesla_ble::management::Status) -> Result<Value, String> {
    let mut value = serde_json::to_value(status).map_err(|_| "Tesla状态编码失败".to_owned())?;
    value["ultra"] = a.hub.observation()["ultra"].clone();
    Ok(value)
}

pub(crate) async fn dispatch(a: &Admin, method: &str, args: &Value) -> Result<Value, String> {
    let command = match method {
        "tesla_status" => return status(a, a.tesla.status()),
        "tesla_ultra_control" => {
            let request: UltraControl = serde_json::from_value(args.clone()).map_err(|_| "原生空调授权仅接受 enabled".to_owned())?;
            a.hub.set_ultra_climate_control(request.enabled, request.session, request.context).map_err(str::to_owned)?;
            return status(a, a.tesla.status());
        }
        "tesla_configure" => {
            let config: Configure = serde_json::from_value(args.clone())
                .map_err(|_| "Tesla配置仅接受VIN和enabled".to_owned())?;
            Command::Configure {
                vin: config.vin.map(|v| v.trim().to_ascii_uppercase()),
                enabled: config.enabled,
            }
        }
        "tesla_connect" | "tesla_retry" => Command::Connect,
        "tesla_disconnect" => Command::Disconnect,
        "tesla_pair" => Command::Pair,
        "tesla_cancel" => Command::Cancel,
        "tesla_climate_pair" => Command::ClimatePair,
        "tesla_climate_query" => Command::ClimateQuery,
        "tesla_climate_command" => {
            let c: ClimateRequest =
                serde_json::from_value(args.clone()).map_err(|_| "空调命令格式无效".to_owned())?;
            use f50_core::tesla::ClimateCommand;
            let command = match (
                c.action.as_str(),
                c.on,
                c.driver_celsius,
                c.passenger_celsius,
            ) {
                ("power", Some(on), None, None) => ClimateCommand::Power { on },
                ("temperature", None, Some(driver_celsius), Some(passenger_celsius)) => {
                    ClimateCommand::SetTemperature {
                        driver_celsius,
                        passenger_celsius,
                    }
                }
                _ => return Err("空调仅支持开关或驾驶/副驾温度".into()),
            };
            Command::ClimateCommand(command)
        }
        _ => return Err("未知Tesla管理方法".into()),
    };
    if !matches!(method, "tesla_configure" | "tesla_climate_command")
        && args.as_object().is_none_or(|m| !m.is_empty())
    {
        return Err("此Tesla管理操作不接受参数".into());
    }
    status(a, a.tesla.command(command).await?)
}
