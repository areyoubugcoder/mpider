/** 表格文字操作：保留键盘、焦点和禁用语义，不占用实体按钮的边框与高度。 */
import { forwardRef, type ButtonHTMLAttributes } from "react";
import { cn } from "@/lib/utils";
export const TextAction = forwardRef<HTMLButtonElement, ButtonHTMLAttributes<HTMLButtonElement>>(({ className, type = "button", ...props }, ref) => <button ref={ref} type={type} className={cn("inline-flex shrink-0 items-center rounded-sm p-0 text-xs leading-5 text-link underline-offset-4 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:text-muted-foreground disabled:no-underline disabled:opacity-60", className)} {...props} />);
TextAction.displayName = "TextAction";
