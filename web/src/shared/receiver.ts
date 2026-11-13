import type {State} from './api';

/** The service's actual advertised name identifies the same receiver on every page. */
export const receiverName = (state: State | null) => state?.network.receiver_name?.trim() || 'CowPlay';
export const receiverPlatform = (state: State | null) => state?.network.platform === 'f50' ? 'F50' : state?.network.platform === 'openwrt' ? 'OpenWrt 主机' : state?.network.platform === 'linux' ? 'Linux 主机' : '接收端';
/** Connection diagrams use a short device label; full broadcast names live in device info. */
export const receiverDevice = (state: State | null) => state?.network.device_name?.trim() || receiverPlatform(state);
export const managementLocation = (platform?: string) => platform === 'f50' || platform === 'openwrt' ? 'OpenWrt › 服务 › CowPlay' : 'CowPlay 主机管理';

/** Vehicle management lives beside the car page's other settings. */
export function teslaManagementUrl(state: State | null): string | null {
  const address = state?.network.origin;
  if (!address) return null;
  try {
    const url = new URL(address);
    if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password) return null;
    return new URL('/car#settings/tesla', url).href;
  } catch { return null; }
}
