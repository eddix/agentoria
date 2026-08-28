import type { Config } from "tailwindcss";
import animate from "tailwindcss-animate";

export default {
  darkMode: ["class"],
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        border: "hsl(var(--border))",
        input: "hsl(var(--input))",
        ring: "hsl(var(--ring))",
        background: "hsl(var(--background))",
        foreground: "hsl(var(--foreground))",
        primary: {
          DEFAULT: "hsl(var(--primary))",
          foreground: "hsl(var(--primary-foreground))",
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
        popover: {
          DEFAULT: "hsl(var(--popover))",
          foreground: "hsl(var(--popover-foreground))",
        },
        card: {
          DEFAULT: "hsl(var(--card))",
          foreground: "hsl(var(--card-foreground))",
        },
      },
      borderRadius: {
        xl: "calc(var(--radius) + 2px)",
        lg: "var(--radius)",
        md: "calc(var(--radius) - 2px)",
        sm: "calc(var(--radius) - 4px)",
      },
      boxShadow: {
        chrome: "0 18px 48px rgba(15, 23, 42, 0.08)",
        panel: "0 8px 24px rgba(15, 23, 42, 0.06)",
      },
      keyframes: {
        floatIn: {
          "0%": { opacity: "0", transform: "translateY(20px)" },
          "100%": { opacity: "1", transform: "translateY(0)" },
        },
      },
      animation: {
        floatIn: "floatIn 480ms ease-out both",
      },
      fontFamily: {
        display: [
          "\"SF Pro Display\"",
          "\"Avenir Next\"",
          "\"Segoe UI\"",
          "\"PingFang SC\"",
          "\"Noto Sans SC\"",
          "sans-serif",
        ],
        body: [
          "\"SF Pro Text\"",
          "\"Avenir Next\"",
          "\"Segoe UI\"",
          "\"PingFang SC\"",
          "\"Noto Sans SC\"",
          "sans-serif",
        ],
        mono: [
          "\"SFMono-Regular\"",
          "\"IBM Plex Mono\"",
          "\"Menlo\"",
          "\"Monaco\"",
          "monospace",
        ],
      },
    },
  },
  plugins: [animate],
} satisfies Config;
