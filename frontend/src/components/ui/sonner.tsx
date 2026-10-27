import { Toaster as Sonner, type ToasterProps } from "sonner";

/**
 * 全局 toast 容器（sonner，shadcn 约定）。挂在 App 根部一次；各处用 `toast.success / error / info / warning`
 * 回显异步操作结果。跟随系统深浅色；右下角不遮挡表格操作列。
 */
function Toaster(props: ToasterProps) {
  return (
    <Sonner
      theme="system"
      position="bottom-right"
      richColors
      closeButton
      duration={4000}
      toastOptions={{
        classNames: {
          toast: "text-xs",
          description: "text-xs",
        },
      }}
      {...props}
    />
  );
}

export { Toaster };
