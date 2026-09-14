declare module "*.png" {
  const url: string;
  export default url;
}

declare module "*.webp" {
  const url: string;
  export default url;
}

declare module "*.svg" {
  const url: string;
  export default url;
}

/* 副作用样式导入（与 vite/client 的语义一致；组件级 CSS 采用 import "./x.css"） */
declare module "*.css" {}
