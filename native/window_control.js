// 已校验版本下的 Windows 页面控制；仅调用 Win32 API，不调用微信内部业务函数。
// 在微信的交互桌面会话执行，Python 调度端不需要模拟 Mac 键盘映射。
'use strict';
const user32 = Process.getModuleByName('user32.dll');
const api = (name, ret, args) => new NativeFunction(user32.getExportByName(name), ret, args);
const getText = api('GetWindowTextW', 'int', ['pointer', 'pointer', 'int']);
const getClass = api('GetClassNameW', 'int', ['pointer', 'pointer', 'int']);
const getPid = api('GetWindowThreadProcessId', 'uint32', ['pointer', 'pointer']);
const visible = api('IsWindowVisible', 'int', ['pointer']);
const iconic = api('IsIconic', 'int', ['pointer']);
const getClient = api('GetClientRect', 'int', ['pointer', 'pointer']);
const clientToScreen = api('ClientToScreen', 'int', ['pointer', 'pointer']);
const foreground = api('GetForegroundWindow', 'pointer', []);
const setForeground = api('SetForegroundWindow', 'int', ['pointer']);
const showWindow = api('ShowWindow', 'int', ['pointer', 'int']);
const setCursor = api('SetCursorPos', 'int', ['int', 'int']);
const sendInput = api('SendInput', 'uint', ['uint', 'pointer', 'int']);
const getDpi = api('GetDpiForWindow', 'uint', ['pointer']);
const enumWindows = api('EnumWindows', 'int', ['pointer', 'pointer']);
let allowedPids = new Set();

function windowInfo(hwnd) {
  const pid = Memory.alloc(4); getPid(hwnd, pid);
  if (!allowedPids.has(pid.readU32()) || !visible(hwnd)) return null;
  const rect = Memory.alloc(16), text = Memory.alloc(2048), cls = Memory.alloc(512);
  if (!getClient(hwnd, rect)) return null;
  const width = rect.add(8).readS32(), height = rect.add(12).readS32();
  getText(hwnd, text, 1024); getClass(hwnd, cls, 256);
  return {hwnd: hwnd.toString(), pid:pid.readU32(), title: text.readUtf16String(), class_name: cls.readUtf16String(),
    width, height, dpi: getDpi(hwnd), minimized: !!iconic(hwnd), foreground: foreground().equals(hwnd)};
}

function checkedWindow(value) {
  const hwnd = ptr(value);
  let info = windowInfo(hwnd);
  if (!info || info.class_name !== 'Chrome_WidgetWin_0') throw new Error('目标窗口无效');
  // 原生浏览器默认可能只有 532 像素宽；最大化后重新读取，保留实际输入坐标。
  showWindow(hwnd, 3);
  setForeground(hwnd);
  if (!foreground().equals(hwnd)) throw new Error('无法取得目标窗口焦点，停止自动操作');
  info = windowInfo(hwnd);
  if (!info || info.width * 96 / info.dpi < 600 || info.height * 96 / info.dpi < 400) throw new Error('目标窗口过小');
  return {hwnd, info};
}

Object.assign(rpc.exports, {
  windows(pids) {
    allowedPids = new Set(pids);
    const result = [];
    const callback = new NativeCallback((hwnd, _) => {
      const value = windowInfo(hwnd);
      if (value && ((value.width >= 400 && value.height >= 400) ||
          (value.minimized && value.class_name === 'Chrome_WidgetWin_0'))) result.push(value);
      return 1;
    }, 'int', ['pointer', 'pointer']);
    enumWindows(callback, ptr(0));
    return result;
  },
  uiAction(value, action, config) {
    const {hwnd, info} = checkedWindow(value);
    const keyboardRefresh=action === 'refresh' && config.refresh_mode === 'ctrl_r';
    const count=keyboardRefresh ? 6 : action === 'advance' ? 4 : 2;
    const inputs = Memory.alloc(count*40); inputs.writeByteArray(new Uint8Array(count*40));
    if (keyboardRefresh) {
      // 新版将刷新移入标签菜单；在 Windows 本机发送 Ctrl+R，不经过远程桌面的键盘映射。
      const point=Memory.alloc(8);
      point.writeS32(Math.round(80*info.dpi/96)); point.add(4).writeS32(Math.round(info.height/2));
      if (!clientToScreen(hwnd,point) || !setCursor(point.readS32(),point.add(4).readS32())) throw new Error('无法定位页面空白');
      inputs.add(20).writeU32(2); inputs.add(60).writeU32(4);
      for (const [offset, scan, flags] of [[80,0x1d,8],[120,0x13,8],[160,0x13,10],[200,0x1d,10]]) {
        inputs.add(offset).writeU32(1);
        inputs.add(offset+10).writeU16(scan);
        inputs.add(offset+12).writeU32(flags);
      }
    } else if (action === 'refresh') {
      const point = Memory.alloc(8), scale = info.dpi / 96;
      point.writeS32(Math.round(config.refresh_x * scale));
      point.add(4).writeS32(Math.round(config.refresh_y * scale));
      if (!clientToScreen(hwnd, point)) throw new Error('无法换算刷新按钮坐标');
      if (!setCursor(point.readS32(), point.add(4).readS32())) throw new Error('无法定位鼠标');
      // x64 INPUT：union 位于 8，MOUSEINPUT.dwFlags 位于 union+12。
      inputs.add(20).writeU32(2); inputs.add(60).writeU32(4);
    } else if (action === 'advance') {
      // 刷新按钮会保留键盘焦点；先点正文左侧空白，防止 End 落在浏览器工具栏。
      const point=Memory.alloc(8);
      point.writeS32(Math.round(80*info.dpi/96)); point.add(4).writeS32(Math.round(info.height/2));
      if (!clientToScreen(hwnd,point) || !setCursor(point.readS32(),point.add(4).readS32())) throw new Error('无法定位页面空白');
      inputs.add(20).writeU32(2); inputs.add(60).writeU32(4);
      // End 扫描码 0x4f，滚到当前列表底部以触发一次翻页；扩展键；成对按下/抬起，避免遗留修饰键。
      for (const offset of [80, 120]) {
        inputs.add(offset).writeU32(1);
        inputs.add(offset + 10).writeU16(0x4f);
        inputs.add(offset + 12).writeU32(offset === 80 ? 9 : 11);
      }
    } else { throw new Error('未知页面操作'); }
    if (sendInput(count, inputs, 40) !== count) throw new Error('页面输入未成功，停止自动操作');
    return {ok: true};
  }
});
