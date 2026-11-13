import {h} from '../shared/dom';
import {confirmAction} from '../shared/confirmation';
import {teslaManagement} from '../shared/tesla-management';
import '../phone/phone.css';
import './manage.css';

const root = document.getElementById('content')!;
let secret = '';
let tesla: ReturnType<typeof teslaManagement> | null = null;
const feedback = h('p', {role: 'status', class: 'lead', id: 'feedback'});
async function api(method: string, args: unknown = {}): Promise<Record<string, any>> {
  const r = await fetch(`/api/manage/${method}`, {method: 'POST', cache: 'no-store', credentials: 'omit', headers: {'Content-Type': 'application/json', Authorization: `Bearer ${secret}`}, body: JSON.stringify(args)});
  const body = await r.json().catch(() => ({}));
  if (!r.ok) throw Error(body.error ?? (r.status === 404 ? '管理入口尚未启用，请在主机上配置管理令牌文件后重启 CowPlay。' : `请求失败 (${r.status})`));
  return body;
}
type Role = 'primary' | 'secondary' | 'destructive';
/** At most one primary action per card; everything else is secondary or destructive. */
function action(label: string, run: () => Promise<void>, role: Role = 'secondary'): HTMLButtonElement {
  const button = h('button', {type: 'button', class: role === 'primary' ? 'primary' : `action ${role}`}, label);
  button.onclick = async () => {
    button.disabled = true; feedback.textContent = '';
    try { await run(); } catch (e) { feedback.textContent = e instanceof Error ? e.message : '操作失败'; }
    finally { button.disabled = false; }
  };
  return button;
}
const field = (label: string, input: HTMLElement) => h('label', {class: 'manage-field'}, h('span', {}, label), input);
function login(): void {
  tesla?.dispose(); tesla = null;
  secret = '';
  const token = h('input', {type: 'password', autocomplete: 'off', required: true, minlength: 64, maxlength: 64, 'aria-label': '管理令牌'});
  const form = h('form', {}, field('管理令牌', token), h('button', {type: 'submit', class: 'primary'}, '进入管理'));
  form.onsubmit = async e => {
    e.preventDefault(); secret = token.value.trim(); token.value = '';
    try { await dashboard(); } catch (e) { secret = ''; feedback.textContent = e instanceof Error ? e.message : '无法进入管理'; }
  };
  document.body.classList.remove('manage-dashboard');
  root.replaceChildren(h('h1', {}, 'CowPlay 管理'), h('p', {class: 'lead'}, '使用主机上的管理令牌登录。车机和手机控制台使用六位访问码，与此令牌无关。'), form, feedback, h('a', {href: '/phone'}, '打开手机控制台'));
}
async function dashboard(): Promise<void> {
  const status = await api('status'), policy = status.policy;
  const presets: [string, string][] = [['720p', '720p · 1280×720'], ['1080p', '1080p · 1920×1080'], ['1440p', '1440p · 2560×1440'], ['2160p', '4K · 3840×2160']];
  const preset = presets.some(([v]) => v === policy.max_res);
  const custom = h('input', {value: preset ? '' : policy.max_res, placeholder: '偶数宽×高，例如 1280x800', 'aria-label': '自定义分辨率'});
  const customField = field('自定义尺寸', custom);
  const resolution = h('select', {'aria-label': '分辨率上限'}, ...presets.map(([v, label]) => h('option', {value: v, selected: v === policy.max_res}, label)), h('option', {value: 'custom', selected: !preset}, '自定义尺寸…'));
  const syncCustom = () => { customField.hidden = resolution.value !== 'custom'; };
  resolution.onchange = syncCustom; syncCustom();
  const fps = h('select', {'aria-label': '帧率上限'}, ...[30, 60].map(n => h('option', {value: n, selected: n === policy.max_fps}, `${n} 帧/秒`)));
  const downgrade = h('input', {type: 'checkbox', checked: policy.auto_downgrade, 'aria-label': '自动降低帧率'});
  const save = action('保存画面上限', async () => {
    const max_res = resolution.value === 'custom' ? custom.value.trim() : resolution.value;
    if (!max_res) throw Error('请填写自定义尺寸，例如 1280x800。');
    if (!await confirmAction({title: '应用画面上限？', description: '当前投屏可能重新连接。超出上限的已有屏幕偏好需要先调整，配对保留。', confirm: '保存并应用'})) return;
    await api('policy_update', {max_res, max_fps: Number(fps.value), auto_downgrade: downgrade.checked});
    feedback.textContent = '画面上限已保存并应用。';
  }, 'primary');
  const tokenValue = h('output', {id: 'screen-token'}, '••••••');
  const showToken = action('显示访问码', async () => { const r = await api('token'); tokenValue.textContent = r.mode === 'open' ? '未启用访问码' : r.mode === 'setup' ? '等待首次选择' : String(r.token || '暂时不可用'); });
  const accessCode=h('input',{type:'password',inputmode:'numeric',maxlength:6,autocomplete:'new-password','aria-label':'新访问码'});
  const changeCode=action('设置访问码',async()=>{if(!/^\d{6}$/.test(accessCode.value))throw Error('请输入六位数字访问码');if(!await confirmAction({title:'设置访问码？',description:'免码进入的浏览器需验证一次；以前验证过的浏览器保留授权。旧码不再用于新增授权。',confirm:'设置'}))return;await api('token_set',{token:accessCode.value});accessCode.value='';tokenValue.textContent='已设置';},'primary');
  const resetCode=action('重置访问码',async()=>{if(!await confirmAction({title:'重置访问码？',description:'将启用新的六位访问码；免码进入的浏览器需验证一次，以前验证过的浏览器保留授权。',confirm:'重置',destructive:true}))return;const r=await api('token',{action:'reset'});tokenValue.textContent=r.token;},'destructive');
  const disableCode=action('不使用访问码',async()=>{if(!await confirmAction({title:'不使用访问码？',description:'能打开此地址的设备可进入投屏和手机管理。已保存的屏幕设置保留。',confirm:'停用访问码',destructive:true}))return;await api('token_disable');accessCode.value='';tokenValue.textContent='未启用访问码';},'destructive');
  const certificate = h('input', {type: 'file', accept: '.pem,.crt', 'aria-label': 'HTTPS 证书链'});
  const key = h('input', {type: 'file', accept: '.pem,.key', 'aria-label': 'HTTPS 私钥'});
  const upload = action('替换 HTTPS 证书', async () => {
    if (!certificate.files?.[0] || !key.files?.[0]) throw Error('请选择证书链和私钥。');
    if (certificate.files[0].size + key.files[0].size > 49152) throw Error('证书链与私钥合计须小于 48 KiB。');
    if (!await confirmAction({title: '替换 HTTPS 证书？', description: '将验证域名、有效期及密钥匹配后应用新证书。验证失败会保留当前证书。', confirm: '验证并替换'})) return;
    try {
      await api('cert_upload', {cert: await certificate.files[0].text(), key: await key.files[0].text()});
      feedback.textContent = 'HTTPS 证书已替换。';
    } finally { certificate.value = ''; key.value = ''; }
  }, 'primary');
  const logout = h('button', {type: 'button', class: 'link-button'}, '退出管理');
  logout.onclick = () => login();
  tesla?.dispose(); tesla = teslaManagement(api);
  tesla.element.classList.add('manage-wide');
  document.body.classList.add('manage-dashboard');
  // Order follows frequency of use: status, access, picture limits, certificate, then the optional vehicle.
  root.replaceChildren(h('header', {class: 'manage-head'}, h('div', {}, h('h1', {}, 'CowPlay 管理'), h('p', {class: 'lead'}, `${status.version ?? '—'} · ${status.build ?? '—'}`)),
    h('nav', {class: 'manage-links'}, h('a', {href: '/car'}, '车机投屏'), h('a', {href: '/phone'}, '手机控制台'), h('a', {href: '#tesla-management'}, 'Tesla 车辆'), logout)), feedback,
    h('div', {class: 'manage-grid'},
      h('section', {}, h('h2', {}, '连接状态'), h('p', {}, status.verdict?.title ?? status.verdict ?? '状态未知'), h('p', {class: 'lead'}, `访问地址：${status.network?.origin ?? '—'}`)),
      h('section', {}, h('h2', {}, '投屏访问'), h('p', {class: 'lead'}, '访问码可选。开启后，新浏览器及免码进入的浏览器需验证一次；不设置时，能打开此地址的设备可直接进入。'),
        h('div', {class: 'manage-field'}, h('span', {}, '当前访问码'), h('span', {class: 'manage-inline'}, tokenValue, showToken)),
        field('新访问码', accessCode), changeCode, h('div', {class: 'manage-danger'}, resetCode, disableCode)),
      h('section', {}, h('h2', {}, '画面上限'), field('分辨率上限', resolution), customField, field('帧率上限', fps), field('自动降低帧率', downgrade), save),
      h('section', {}, h('h2', {}, 'HTTPS 证书'), h('p', {class: 'lead'}, status.network?.cert_days == null ? '有效期未知' : `证书剩余 ${status.network.cert_days} 天`), field('证书链 PEM', certificate), field('私钥 PEM', key), upload),
      tesla.element),
    h('p', {class: 'lead'}, '热点、路由、开机启动和升级由主机系统管理。手机控制台提供配对、重连、屏幕偏好与诊断日志。'));
  tesla.start();
}
window.addEventListener('pagehide', () => { tesla?.dispose(); });
login();
