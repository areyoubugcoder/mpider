import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/** shadcn 标准 className 合并工具：clsx 条件拼接 + tailwind-merge 消除冲突类。 */
export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}

/** 统一把未知错误转成字符串（对齐旧 main.ts 的 msg()）。 */
export function msg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
