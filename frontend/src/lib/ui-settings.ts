/** 本机界面偏好：实验室默认隐藏，设置保存成功后立即更新导航。 */
export const LAB_SETTING_KEY = "mpider-show-laboratory";
export function laboratoryVisible(): boolean {
  try { return localStorage.getItem(LAB_SETTING_KEY) === "true"; }
  catch { return false; }
}
export function saveLaboratoryVisible(visible: boolean): void {
  localStorage.setItem(LAB_SETTING_KEY, String(visible));
}
