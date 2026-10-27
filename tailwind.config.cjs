/** @type {import('tailwindcss').Config} */
// CommonJS（.cjs）：仓库根 package.json 是 `type: module`，用 .cjs 让 require/module.exports 生效，
// 避免 ESM/CJS 混用。content 路径相对 cwd（= 仓库根，pnpm 脚本从这里跑）解析。
// darkMode: "media" —— 主题跟随系统明暗色（需求 §9）。
// 视觉基线（色彩 / 字体 / 字号标尺）见 docs/ui-baseline.md，token 定义在 frontend/src/index.css。
module.exports = {
  darkMode: "media",
  content: ["./frontend/index.html", "./frontend/src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        border: "hsl(var(--border))",
        "border-muted": "hsl(var(--border-muted))",
        link: "hsl(var(--link))",
        input: "hsl(var(--input))",
        ring: "hsl(var(--ring))",
        background: "hsl(var(--background))",
        foreground: "hsl(var(--foreground))",
        primary: {
          DEFAULT: "hsl(var(--primary))",
          foreground: "hsl(var(--primary-foreground))",
          hover: "hsl(var(--primary-hover))",
        },
        secondary: {
          DEFAULT: "hsl(var(--secondary))",
          foreground: "hsl(var(--secondary-foreground))",
        },
        destructive: {
          DEFAULT: "hsl(var(--destructive))",
          foreground: "hsl(var(--destructive-foreground))",
        },
        muted: {
          DEFAULT: "hsl(var(--muted))",
          foreground: "hsl(var(--muted-foreground))",
        },
        accent: {
          DEFAULT: "hsl(var(--accent))",
          foreground: "hsl(var(--accent-foreground))",
        },
        card: {
          DEFAULT: "hsl(var(--card))",
          foreground: "hsl(var(--card-foreground))",
        },
        popover: {
          DEFAULT: "hsl(var(--popover))",
          foreground: "hsl(var(--popover-foreground))",
        },
        // 侧栏（shadcn 标准 sidebar 令牌集，随明暗主题）
        sidebar: {
          DEFAULT: "hsl(var(--sidebar-background))",
          foreground: "hsl(var(--sidebar-foreground))",
          primary: "hsl(var(--sidebar-primary))",
          "primary-foreground": "hsl(var(--sidebar-primary-foreground))",
          accent: "hsl(var(--sidebar-accent))",
          "accent-foreground": "hsl(var(--sidebar-accent-foreground))",
          border: "hsl(var(--sidebar-border))",
          ring: "hsl(var(--sidebar-ring))",
        },
        // 状态语义色（证书/代理/微信等圆点与徽章）
        success: {
          DEFAULT: "hsl(var(--success))",
          foreground: "hsl(var(--success-foreground))",
        },
        warning: {
          DEFAULT: "hsl(var(--warning))",
          foreground: "hsl(var(--warning-foreground))",
        },
      },
      borderRadius: {
        lg: "var(--radius)",
        md: "calc(var(--radius) - 2px)",
        sm: "calc(var(--radius) - 4px)",
      },
      // 字体：Inter 承担 UI 文本；JetBrains Mono 只给机器可读文本（biz / key / 路径 / 日志）
      fontFamily: {
        sans: [
          '"Inter Variable"',
          "Inter",
          "-apple-system",
          "BlinkMacSystemFont",
          '"Segoe UI"',
          '"PingFang SC"',
          '"Microsoft YaHei"',
          "sans-serif",
        ],
        mono: [
          '"JetBrains Mono Variable"',
          '"JetBrains Mono"',
          "ui-monospace",
          "SFMono-Regular",
          "Menlo",
          "Consolas",
          "monospace",
        ],
      },
      // 字号标尺 13 / 15 / 17 / 21 / 27 / 35：正文 sm=15，表格 / 说明 xs=13，页面标题 xl=27
      fontSize: {
        xs: ["13px", { lineHeight: "1.25rem" }],
        sm: ["15px", { lineHeight: "1.375rem" }],
        base: ["17px", { lineHeight: "1.625rem" }],
        lg: ["21px", { lineHeight: "1.75rem" }],
        xl: ["27px", { lineHeight: "2.125rem" }],
        "2xl": ["35px", { lineHeight: "2.5rem" }],
      },

      keyframes: {
        "accordion-down": {
          from: { height: "0" },
          to: { height: "var(--radix-accordion-content-height)" },
        },
        "accordion-up": {
          from: { height: "var(--radix-accordion-content-height)" },
          to: { height: "0" },
        },
      },
      animation: {
        "accordion-down": "accordion-down 0.2s ease-out",
        "accordion-up": "accordion-up 0.2s ease-out",
      },
    },
  },
  plugins: [require("tailwindcss-animate"), require("@tailwindcss/typography")],
};
