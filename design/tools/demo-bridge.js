(() => {
  if(new URLSearchParams(location.search).get("demo")!=="1")return;
  const meta=window.PROPOSAL;
  const send=(type,payload={})=>parent.postMessage({source:"r-code-final-demo",type,...payload},"*");
  const navigate=page=>send("navigate",{page});
  const toast=message=>send("toast",{message});
  const clean=value=>String(value||"").replace(/\s+/g," ").trim();
  const settingsRoutes={"模型服务":"settings-providers","Agent 编排":"settings-agents","子代理配置":"settings-subagents","工具与连接":"settings-tools","知识与指令":"settings-knowledge","权限":"settings-permissions","隐私与安全":"settings-security","外观与小助手":"settings-appearance","通知":"settings-notifications","启动与关闭":"settings-lifecycle","更新":"settings-updates","诊断":"settings-diagnostics"};
  const sidebarRoutes={"新对话":"home","项目概览":"dashboard","所有对话":"conversations","待处理":"inbox","活动":"activity","归档":"archive","项目":"projects","设置":"settings-providers"};
  const toolRoutes={"运行与子代理":"runs","终端":"terminal","文件":"files","审核":"review","计划":"plan"};
  const actionRoutes={"查看请求":"permission","查看差异":"review","进入审核":"review","项目文件":"editor","新建任务":"home","新对话":"home","快速打开":"files","回到对话补充":"conversation"};
  function findRoute(target){
    const button=target.closest("button,a");if(!button)return null;const label=clean(button.textContent);
    if(button.closest(".opt-settings-nav"))return settingsRoutes[label]||null;
    if(button.closest(".app-sidebar")){
      for(const [name,route] of Object.entries(sidebarRoutes))if(label===name||label.startsWith(name))return route;
    }
    if(button.closest(".opt-tool-line")){
      for(const [name,route] of Object.entries(toolRoutes))if(label.includes(name))return route;
    }
    for(const [name,route] of Object.entries(actionRoutes))if(label===name||label.startsWith(name))return route;
    return null;
  }
  document.addEventListener("click",event=>{
    const target=event.target instanceof Element?event.target:null;if(!target)return;
    const route=findRoute(target);if(route){event.preventDefault();navigate(route);return;}
    const button=target.closest("button");if(!button)return;const label=clean(button.textContent);
    if(button.classList.contains("opt-toggle")){button.classList.toggle("on");button.setAttribute("aria-label",button.classList.contains("on")?"已开启":"已关闭");toast(button.classList.contains("on")?"已开启演示状态":"已关闭演示状态");return;}
    if(button.closest(".opt-tabs")){button.parentElement.querySelectorAll("button").forEach(item=>item.classList.toggle("active",item===button));return;}
    if(button.classList.contains("opt-theme-choice")){send("theme",{theme:button.querySelector(".opt-theme-window")?.classList.contains("light")?"light":"dark"});return;}
    if(button.classList.contains("opt-suggestion")){
      const textarea=document.querySelector(".opt-home textarea,.home-composer textarea");if(textarea){textarea.value=label;textarea.dispatchEvent(new Event("input",{bubbles:true}));textarea.focus();toast("示例已填入草稿");}return;
    }
    if(meta.id==="search-empty"&&label.startsWith("清空搜索")){navigate("search");return;}
    if(meta.id==="search"&&label==="×"){navigate("home");return;}
    if(meta.id==="provider-discard"&&label.startsWith("继续编辑")){navigate("provider-editor");return;}
    if(meta.id==="provider-discard"&&label.startsWith("放弃更改")){toast("已放弃草稿并恢复上次保存的配置");navigate("settings-providers");return;}
    if(meta.id==="provider-editor"&&label==="取消"){navigate("provider-discard");return;}
    if(meta.id==="provider-editor"&&label==="×"){navigate("provider-discard");return;}
    if(meta.id==="provider-editor"&&label.startsWith("保存更改")){toast("演示：模型服务更改已保存");navigate("settings-providers");return;}
    if(["允许一次","拒绝","接受变更","请求修改"].some(name=>label.startsWith(name))){button.disabled=true;toast(`演示操作：${label}`);return;}
    if(label==="×"&&["runs","files","terminal","review","plan","tool-launcher"].includes(meta.id)){navigate("conversation");return;}
    if(label&&!button.disabled)toast(`演示控件：${label}`);
  });
  document.addEventListener("change",event=>{
    const target=event.target;if(target instanceof HTMLSelectElement||target instanceof HTMLInputElement)toast("演示状态已更新");
  });
  window.addEventListener("keydown",event=>{
    if(event.key==="Escape"){
      if(meta.id==="provider-discard")navigate("provider-editor");
      else if(meta.id==="provider-editor")navigate("settings-providers");
      else if(meta.id.startsWith("search"))navigate("home");
      else if(["runs","files","terminal","review","plan","tool-launcher"].includes(meta.id))navigate("conversation");
    }
    if(event.ctrlKey&&event.key.toLowerCase()==="k"){event.preventDefault();navigate("search");}
    if(event.ctrlKey&&event.key.toLowerCase()==="p"){event.preventDefault();navigate("files");}
    if(event.ctrlKey&&event.key==="`"){event.preventDefault();navigate("terminal");}
    if(event.ctrlKey&&event.shiftKey&&event.key.toLowerCase()==="g"){event.preventDefault();navigate("review");}
  });
  send("ready",{page:meta.id});
})();
