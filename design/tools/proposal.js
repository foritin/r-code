// Render-only layouts for PNG review. These are not application changes.
(() => {
  const meta = window.PROPOSAL;
  const theme = new URLSearchParams(location.search).get("theme") === "light" ? "studio-light" : "obsidian";
  document.documentElement.dataset.theme = theme;
  document.body.classList.add("proposal");
  document.body.dataset.proposal = meta.id;
  document.body.dataset.motion = new URLSearchParams(location.search).get("motion") === "1" ? "on" : "off";
  const app = document.querySelector("#app");
  app.style.setProperty("--rc-current-rail", "264px");
  app.style.setProperty("--rc-rail-w", "264px");
  const main = document.querySelector("#main-content");
  const esc = value => String(value ?? "").replace(/[&<>"']/g, c => ({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));
  const paths = {
    folder:'<path d="M3 7h6l2 2h10v11H3z"/><path d="M3 7V4h7l2 3"/>',
    file:'<path d="M6 3h8l4 4v14H6z"/><path d="M14 3v5h4M9 12h6M9 16h5"/>',
    code:'<path d="m8 6-5 6 5 6m8-12 5 6-5 6M14 4l-4 16"/>',
    terminal:'<rect x="3" y="4" width="18" height="16" rx="3"/><path d="m7 9 3 3-3 3m6 0h4"/>',
    search:'<circle cx="10.5" cy="10.5" r="6.5"/><path d="m16 16 5 5"/>',
    chart:'<path d="M4 20h17M7 16V9m5 7V4m5 12v-5"/>',
    shield:'<path d="m12 3 8 3v6c0 4-4 7-8 9-4-2-8-5-8-9V6z"/><path d="m8 12 3 3 5-6"/>',
    plus:'<path d="M12 5v14M5 12h14"/>',
    arrow:'<path d="M4 12h15m-6-6 6 6-6 6"/>',
    chevron:'<path d="m9 6 6 6-6 6"/>',
    down:'<path d="m6 9 6 6 6-6"/>',
    close:'<path d="m6 6 12 12M6 18 18 6"/>',
    refresh:'<path d="M20 8a8 8 0 0 0-14-3L3 8m0-5v5h5m-4 8a8 8 0 0 0 14 3l3-3m0 5v-5h-5"/>',
    layers:'<path d="m12 3 10 6-10 6L2 9zm-9 11 9 5 9-5m-18 4 9 5 9-5"/>',
    branch:'<circle cx="6" cy="5" r="2"/><circle cx="18" cy="7" r="2"/><circle cx="6" cy="20" r="2"/><path d="M6 7v11m0-5c8 0 12-1 12-4"/>',
    check:'<path d="m5 12 4 4L19 6"/>',
    clock:'<circle cx="12" cy="12" r="9"/><path d="M12 7v6l4 2"/>',
    lock:'<rect x="5" y="10" width="14" height="11" rx="2"/><path d="M8 10V7a4 4 0 0 1 8 0v3m-4 4v3"/>',
    info:'<circle cx="12" cy="12" r="9"/><path d="M12 11v6m0-10v1"/>',
    message:'<path d="M4 4h16v13H9l-5 4z"/><path d="M8 8h8M8 12h5"/>',
    agent:'<rect x="5" y="7" width="14" height="12" rx="3"/><path d="M12 3v4M2 11h3m14 0h3M8 19v2m8-2v2M9 11v3m6-3v3"/>',
  };
  const icon = (name) => `<span class="opt-icon" aria-hidden="true"><svg viewBox="0 0 24 24">${paths[name] ?? paths.file}</svg></span>`;
  const button = (label, kind = "", name = "", disabled = false) => `<button type="button" class="opt-button ${kind}"${disabled ? " disabled" : ""}>${name ? icon(name) : ""}${label}</button>`;
  const pill = (label, tone = "") => `<span class="opt-pill ${tone}">${label}</span>`;
  const toggle = (on = false) => `<span class="opt-toggle${on ? " on" : ""}" role="img" aria-label="${on ? "已开启" : "已关闭"}"></span>`;
  const actions = html => `<div class="opt-actions">${html}</div>`;
  const header = (title, description, action = "") => `<header class="opt-page-head"><div><h1>${title}</h1><p>${description}</p></div>${action ? actions(action) : ""}</header>`;
  const card = (title, body, action = "", desc = "") => `<section class="opt-card">${title || action || desc ? `<header class="opt-card-head"><div><h2>${title}</h2>${desc ? `<p>${desc}</p>` : ""}</div>${action}</header>` : ""}${body}</section>`;
  const field = (label, control, help = "") => `<label class="opt-field"><span>${label}</span>${control}${help ? `<small class="opt-help">${help}</small>` : ""}</label>`;
  const input = (value = "", placeholder = "", extra = "") => `<input class="opt-input" value="${esc(value)}" placeholder="${esc(placeholder)}" ${extra}>`;
  const select = value => `<select class="opt-input"><option>${value}</option></select>`;
  const textarea = (value = "", placeholder = "", extra = "") => `<textarea class="opt-input" placeholder="${esc(placeholder)}" ${extra}>${esc(value)}</textarea>`;
  const tabs = (labels, current = 0, extra = "") => `<div class="opt-tabs ${extra}">${labels.map((label,i)=>`<button type="button" class="${i === current ? "active" : ""}">${label}</button>`).join("")}</div>`;
  const notice = (text, tone = "", action = "") => `<div class="opt-notice ${tone}">${icon(tone === "warning" ? "info" : "shield")}<div>${text}</div>${action}</div>`;
  const empty = (title, text, action = "", name = "file") => `<div class="opt-empty">${icon(name)}<h3>${title}</h3><p>${text}</p>${action}</div>`;
  const providerIcon = name => name === "relay" ? '<span class="opt-provider-icon">R</span>' : `<span class="opt-provider-icon"><img src="../icons/${name}.png" alt=""></span>`;
  const sectionHead = (title, action = "") => `<div class="opt-section-head"><h2>${title}</h2>${action}</div>`;
  const fileTree = selected => `<aside class="opt-file-tree"><h3>R-CODE</h3>${[["folder","src"],["folder","assets"],["file","Cargo.toml"],["file","README.md"]].map(([name,label])=>`<div class="${label===selected?"selected":""}">${icon(name)}${label}</div>`).join("")}</aside>`;
  const kv = values => `<dl class="opt-kv">${values.map(([k,v])=>`<dt>${k}</dt><dd>${v}</dd>`).join("")}</dl>`;
  const row = (title, text, action = "", name = "") => `<div class="opt-row">${name ? icon(name) : ""}<div class="opt-grow"><strong>${title}</strong>${text ? `<small>${text}</small>` : ""}</div>${action}</div>`;

  const settingGroups = [
    ["模型与能力", [["providers","模型服务"],["agents","Agent 编排"],["subagents","子代理配置"],["tools","工具与连接"]]],
    ["项目与安全", [["knowledge","知识与指令"],["permissions","权限"],["security","隐私与安全"]]],
    ["应用", [["appearance","外观与小助手"],["notifications","通知"],["lifecycle","启动与关闭"],["updates","更新"],["diagnostics","诊断"]]],
  ];
  function settings(title, pane, description, body, action = "") {
    main.innerHTML = `<section class="opt-settings"><nav class="opt-settings-nav" aria-label="设置"><h2>设置</h2>${settingGroups.map(([label,items])=>`<span class="group-label">${label}</span>${items.map(([id,name])=>`<button class="${id===pane?"active":""}">${name}</button>`).join("")}`).join("")}</nav><div class="opt-settings-main"><div class="opt-content"><div class="opt-settings-topline"><span>设置 / ${title}</span><div class="opt-search-field">${icon("search")}<span>搜索设置</span></div></div>${header(title,description,action)}${body}</div></div></section>`;
  }

  function home() {
    const composer = main.querySelector(".home-composer")?.outerHTML || window.HOME_COMPOSER;
    main.innerHTML = `<section class="opt-home"><span class="opt-eyebrow">R-CODE / 新任务</span><h1>开始一个新任务</h1><p>描述你的目标，或附加项目，让 R-Code 和你一起完成。</p>${composer}<div class="opt-suggestions"><button class="opt-suggestion">${icon("code")}定位并修复失败测试</button><button class="opt-suggestion">${icon("branch")}理解模块调用关系</button><button class="opt-suggestion">${icon("shield")}审核当前代码变更</button></div><div class="opt-home-foot"><span>示例只会填入草稿，你可以继续修改。</span><span>Enter 发送 · Shift + Enter 换行</span></div></section>`;
  }

  const taskRows = [
    ["优化 Rust 编译性能","等待授权 · cargo test","r-code","等待批准","warning","4m"],
    ["统一错误处理规范","错误边界与测试已补齐","r-code","等待审核","accent","8m"],
    ["修复任务队列并发问题","分析实现与关联模块","r-code","正在执行","success","2m"],
    ["添加请求限流中间件","分析实现与关联模块","api-server","正在分析","success","6m"],
    ["更新依赖并修复告警","分析实现与关联模块","r-code","已完成","","26m"],
  ];
  const taskTable = rows => `<table class="opt-table"><colgroup><col style="width:45%"><col style="width:20%"><col style="width:20%"><col style="width:15%"></colgroup><thead><tr><th>任务</th><th>项目</th><th>状态</th><th class="opt-last">最近更新</th></tr></thead><tbody>${rows.map(r=>`<tr><td><strong>${r[0]}</strong><small>${r[1]}</small></td><td class="opt-muted">${r[2]}</td><td>${pill(r[3],r[4])}</td><td class="opt-last opt-faint">${r[5]}　⋯</td></tr>`).join("")}</tbody></table>`;
  function dashboard() {
    main.innerHTML = `<section class="opt-page">${header("r-code","当前项目的任务、待处理事项与最近动态。",button("项目文件","","folder")+button("新建任务","primary","plus"))}<div class="opt-dashboard"><div><div class="opt-summary"><span><b>2</b>待处理</span><span><b>1</b>运行中</span><span><b>3</b>子代理</span><span>4 个项目任务</span></div>${sectionHead("需要你处理",button("查看全部","quiet","arrow"))}<div class="opt-decision-row">${icon("shield")}<div class="opt-grow">${pill("权限请求","warning")}<h3>优化 Rust 编译性能</h3><p><span class="opt-mono">cargo test</span> · 等待 4 分钟</p></div>${button("查看请求","primary","arrow")}</div><div class="opt-decision-row">${icon("file")}<div class="opt-grow">${pill("等待审核","accent")}<h3>统一错误处理规范</h3><p>2 个变更文件 · 等待 8 分钟</p></div>${button("查看差异","","arrow")}</div>${sectionHead("项目任务",'<small>未归档对话</small>')}${taskTable(taskRows.filter(r=>r[2]==="r-code"))}<div class="opt-footer"><span>归档中暂无对话</span>${button("项目记忆","quiet","layers")}</div></div><aside class="opt-dashboard-aside"><h2>最近动态</h2>${[["完成了一次执行","统一错误处理规范","8 分钟前"],["创建了任务","优化 Rust 编译性能","20 分钟前"],["完成了一次执行","更新依赖并修复告警","26 分钟前"],["创建了任务","修复任务队列并发问题","36 分钟前"]].map(r=>`<div class="opt-feed-item"><strong>${r[0]}</strong><small>${r[1]}</small><time>${r[2]}</time></div>`).join("")}</aside></div></section>`;
  }
  function conversations() {
    main.innerHTML = `<section class="opt-page">${header("所有对话","在项目之间切换，继续正在进行的工作。",button("新对话","primary","plus"))}<div class="opt-toolbar"><div class="opt-search-field">${icon("search")}<span>搜索任务或项目</span></div>${tabs(["全部","运行中","待处理","待审核","已完成","已归档"])}</div>${taskTable(taskRows)}<div class="opt-footer"><span>5 个对话 · 按最近活动排列</span><span>选中任务可继续对话或查看详情</span></div></section>`;
  }
  function projects() {
    const projects = [
      {name:"r-code",path:"D:/project/rust/r-code",current:true,policy:"按风险审批",count:4},
      {name:"api-server",path:"D:/project/rust/api-server",current:false,policy:"请求审批",count:1},
    ];
    main.innerHTML = `<section class="opt-page opt-projects-page">
      ${header("项目","从熟悉的工作区继续。",button("选择文件夹","primary","plus"))}
      <section class="opt-projects-list"><div class="opt-list-caption"><span>最近使用 <b>02</b></span><span>会话</span><span>访问策略</span><span></span></div>
      ${projects.map(p=>`<div class="opt-project-line">${icon("folder")}<div class="opt-project-title"><strong>${p.name}</strong>${p.current?'<span class="opt-current-label">当前项目</span>':""}<small class="opt-mono">${p.path}</small></div><span class="opt-project-count">${p.count}<small>个会话</small></span><span class="opt-project-policy">${icon("shield")}${p.policy}</span><div class="opt-actions">${button("打开","quiet","arrow")}${button("⋯","quiet")}</div></div>`).join("")}</section>
      <footer class="opt-projects-footer"><span>项目文件与知识设置，收在每行的更多菜单中。</span><span>移除项目会保留磁盘文件</span></footer>
    </section>`;
  }
  function editor() {
    main.innerHTML = `<section class="opt-editor"><header class="opt-panel-head">${icon("folder")}<h2>r-code</h2><div class="opt-search-field" style="min-width:280px">${icon("search")}<span>快速打开文件</span><kbd>Ctrl K</kbd></div></header><div class="opt-file-layout">${fileTree("Cargo.toml")}<div class="opt-editor-code"><div class="opt-file-tab">${icon("file")}<strong>Cargo.toml</strong>${pill("只读预览")}${button("编辑文件","","code")}</div><pre><span class="opt-line-no">1</span><span class="opt-code-key">[workspace]</span>\n<span class="opt-line-no">2</span>resolver = <span class="opt-code-value">"2"</span>\n<span class="opt-line-no">3</span>members = [<span class="opt-code-value">"crates/r-code-core"</span>, <span class="opt-code-value">"src-tauri"</span>]\n<span class="opt-line-no">4</span></pre><div class="opt-code-foot"><span>4 行 · Cargo.toml</span><span>只读预览 · 点击编辑后可修改并保存</span></div></div></div></section>`;
  }

  function panelShell(title, content, footer = "", cls = "") {
    return `<header class="opt-panel-head">${icon({"运行与子代理":"chart","终端":"terminal","审核":"shield","计划":"code","文件":"file","任务工具":"layers"}[title])}<h2>${title}</h2>${button("聚焦","quiet")}${button("×","quiet")}</header><div class="opt-panel-body ${cls}">${content}</div>${footer ? `<footer class="opt-panel-footer">${footer}</footer>` : ""}`;
  }
  // The name is an independent assigned display name, not the agent type.
  // These short names are proposal fixtures; summaries retain the repository's sample work.
  const subagentSamples = [
    {name:"boundary-check",summary:"检查任务队列与调度器实现",state:"running"},
    {name:"plan-verifier",summary:"把并发修复拆成可验证步骤",state:"running"},
    {name:"test-runner",summary:"运行 supervisor 定向测试",state:"running",child:true},
    {name:"lock-review",summary:"核对跨 await 的持锁顺序",state:"complete"},
    {name:"reviewer",summary:"只读复核已完成",state:"complete"},
  ];
  function subagentLines({all=false,panel=false}={}) {
    const agents = all ? subagentSamples : subagentSamples.filter(a=>a.state==="running");
    return `<div class="opt-subagent-lines${panel?" in-panel":""}" aria-label="子代理执行明细">${agents.map((a,i)=>`<div class="opt-subagent-line${a.child&&panel?" is-child":""}" data-agent-state="${a.state}" style="--agent-delay:${i*-.8}s">${icon(a.state==="complete"?"check":"agent")}<span class="opt-agent-kind">子智能体</span><span class="opt-agent-name${a.state==="running"?" is-running":" is-complete"}" data-name="${a.name}">${a.name}</span><span class="opt-agent-separator">·</span><span class="opt-agent-description">${a.summary}</span></div>`).join("")}${!all?'<div class="opt-subagent-tail"><span>另 2 个子代理已完成</span><span>查看全部　↗</span></div>':""}</div>`;
  }
  function runPanel() {
    return panelShell("运行与子代理",card("修复任务队列并发问题",'<p class="opt-help">等待模型响应 · 当前运行已持续 36 分钟</p><div class="opt-mini-stats"><div><strong>3</strong><span>运行中子代理</span></div><div><strong>2</strong><span>已完成子代理</span></div><div><strong>2</strong><span>变更文件</span></div></div><div class="opt-help">工具活动　3 条命令 · 1 次写入</div>',pill("运行中","success"))+sectionHead("子代理",'<small>5 个 · 按任务层级</small>')+subagentLines({all:true,panel:true})+sectionHead("关键事件")+row("命令执行完成",'<span class="opt-mono">rg -n "Mutex|RwLock|await" src</span>',"","terminal")+row("检查并发边界","Codex CLI · 仍在执行",pill("3 分钟前"),"code")+'<div class="opt-footer"><span>原始审计流</span><span>展开查看　⌄</span></div>');
  }
  function toolsPanel() {
    const tools = [["chart","运行与子代理","当前运行、结果与子代理","Ctrl Alt S"],["terminal","终端","任务内的终端会话","Ctrl `"],["file","文件","项目文件与代码","Ctrl P"],["shield","审核","差异、验证与接受变更","Ctrl Shift G"],["code","计划","步骤、进展与待确认问题",""]];
    return panelShell("任务工具",'<div class="opt-tool-intro"><span>在对话旁展开</span><p>为当前任务选择一个工具。</p></div><nav class="opt-tool-menu" aria-label="任务工具">'+tools.map(t=>`<button class="opt-tool-line" type="button">${icon(t[0])}<span class="opt-tool-copy"><strong>${t[1]}${t[1]==="审核"?'<em>2</em>':""}</strong><small>${t[2]}</small></span><kbd>${t[3]}</kbd>${icon("chevron")}</button>`).join("")+"</nav><div class=\"opt-tool-footnote\">工具状态随任务保留。收起面板后，任务继续运行。</div>");
  }
  function terminalPanel() {
    return panelShell("终端",`<div class="opt-terminal-tabs">${button("PowerShell 1","active","terminal")}${button("+","quiet")}<span style="margin-left:auto">${pill("空闲")}</span></div><div class="opt-terminal-path">D:\\project\\rust\\r-code</div><pre>R-Code Demo Terminal\nPowerShell 7\n\n<span class="opt-code-value">PS</span> D:\\project\\rust\\r-code&gt; <span style="color:var(--accent)">▌</span></pre>`,'<div class="opt-help" style="margin:0">当前任务终端 · 切换标签不会中断会话</div>',"opt-terminal");
  }
  function filesPanel() {
    return panelShell("文件",`<div class="opt-terminal-path">r-code　/　项目文件</div><div class="opt-file-layout">${fileTree("")}<div class="opt-file-blank">${icon("file")}<h3>打开一个文件</h3><p>从目录中选择，或使用快速打开定位文件。</p>${button("快速打开","","search")}</div></div>`,"","opt-terminal");
  }
  function reviewPanel() {
    const diff = `<pre class="opt-diff"><span class="opt-faint">@@ -1,3 +1,4 @@</span>\n  pub async fn health() -&gt; &amp;'static str {\n<span class="opt-del">− // previous demo implementation</span><span class="opt-add">+ // complete interactive demo implementation</span><span class="opt-add">+ // shares the production React UI</span></pre>`;
    return panelShell("审核",'<div class="opt-check-steps"><strong>1 检查差异</strong><i></i><span>2 验证</span><i></i><span>3 决定</span></div>'+row("统一错误处理规范","2 个文件 · 等待审核",pill("待审核","accent"))+`<div class="opt-review-files"><div class="opt-review-file selected">${icon("file")}src/api.rs<small>+2 −1</small></div>${diff}<div class="opt-review-file">${icon("file")}src/error.rs<small>+2 −1</small></div></div>`+sectionHead("验证结果",button("重新验证","quiet","refresh"))+row("cargo test","退出码 0 · 耗时 60 秒",pill("通过","success"),"check")+notice("接受变更不会自动提交或推送代码。")+'<div class="opt-footer"><span>Git 交付</span><span>审核完成后可用　⌄</span></div>',actions(button("请求修改")+button("更多操作　⌄","quiet")+button("接受变更","primary","check"))+'<p class="opt-help">先确认差异，再接受本次变更。回滚位于更多操作中。</p>');
  }
  function planPanel() {
    return panelShell("计划",row("任务计划","修订 1 · 尚未确认",pill("草稿"),"code")+kv([["目标","梳理任务队列执行路径并修复并发状态竞争。"],["文档",'<span class="opt-mono">plan.md</span>']])+empty("计划尚无步骤","补充需要完成的范围和验收要求，再继续整理计划。",button("回到对话补充","primary","message"),"code"));
  }
  function permissionPanel() {
    return panelShell("运行与子代理",row("优化 Rust 编译性能","当前运行已暂停",pill("等待批准","warning"))+card("运行项目测试",'<p class="opt-help">主代理请求运行测试，以验证本次修改。</p>'+kv([["命令",'<code class="opt-mono">cargo test</code>'],["工作目录",'<span class="opt-mono">D:/project/rust/r-code</span>'],["本次授权","仅允许这一次请求"]])+actions(button("拒绝","danger")+button("允许一次","primary","check"))+'<div class="opt-footer"><span>本任务内持续允许</span><span>查看范围并确认　⌄</span></div>',pill("R2 · 需要确认","warning"))+sectionHead("任务状态")+row("等待权限处理","批准或拒绝后，任务状态会同步更新。", "", "clock")+row("会话已创建","20 分钟前", "", "message")+'<div class="opt-footer"><span>原始审计流</span><span>展开　⌄</span></div>');
  }
  function room(id) {
    document.querySelectorAll(".timeline-subagent-event").forEach(el=>el.innerHTML=subagentLines());
    if(id === "conversation") return;
    const workspace = main.querySelector(".scene-room");
    workspace.style.setProperty("--opt-panel-width",["terminal","files","review"].includes(id)?"560px":"500px");
    const canvas = workspace.querySelector(".canvas.workbench");
    const render = {"tool-launcher":toolsPanel,runs:runPanel,terminal:terminalPanel,files:filesPanel,review:reviewPanel,plan:planPanel,permission:permissionPanel}[id];
    canvas.innerHTML = render();
    if(id === "permission") {
      const stack = main.querySelector(".convo .perm-stack");
      if(stack)stack.innerHTML=notice('<strong>任务已暂停</strong><br>等待你处理 cargo test 的权限请求。',"warning",button("查看请求","quiet","arrow"));
      const trace = main.querySelector(".convo .timeline-process-disclosure");
      if(trace)trace.innerHTML="等待批准 · 1 项操作　›";
      const composer = main.querySelector(".convo textarea");
      if(composer)composer.placeholder="等待授权，可继续补充说明…";
    }
  }
  function inbox() {
    main.innerHTML = `<section class="opt-inbox opt-inbox-refined"><div class="opt-inbox-list">
      ${header("待处理","2 项决策，完成后继续工作。")}
      <div class="opt-inbox-toolbar">${tabs(["全部 2","授权 1","审核 1"])}<span>按项目分组</span></div>
      <header class="opt-inbox-project">${icon("folder")}<strong>r-code</strong><span>D:/project/rust/r-code</span></header>
      <div class="opt-inbox-table"><div class="opt-inbox-columns"><span>事项</span><span>类型</span><span>等待</span><span></span></div>
      <button class="opt-inbox-line selected" type="button">${icon("file")}<span class="opt-inbox-subject"><strong>统一错误处理规范</strong><small>2 个文件待处理</small></span><span class="opt-inline-state review">待审核</span><time>8 分钟</time>${icon("chevron")}</button>
      <button class="opt-inbox-line" type="button">${icon("shield")}<span class="opt-inbox-subject"><strong>优化 Rust 编译性能</strong><small class="opt-mono">cargo test</small></span><span class="opt-inline-state approval">待授权</span><time>4 分钟</time>${icon("chevron")}</button></div>
      <div class="opt-inbox-list-foot">1 个项目 · 所有处理结果会同步回任务</div>
      </div><aside class="opt-inbox-detail"><header class="opt-panel-head"><h2>审核摘要</h2>${button("×","quiet")}</header><div class="opt-panel-body">
      <div class="opt-inbox-detail-title"><span>r-code / 当前选择</span><h2>统一错误处理规范</h2><p>检查差异后，再决定是否接受。</p></div>
      <div class="opt-validation-line">${icon("check")}<strong>cargo test</strong><span>验证通过</span></div>
      <div class="opt-inbox-files"><div class="opt-file-list-caption"><span>变更文件</span><span>2</span></div>${["src/error.rs","src/api.rs"].map(file=>`<button class="opt-inbox-file" type="button">${icon("file")}<span>${file}</span><small>修改</small>${icon("arrow")}</button>`).join("")}</div>
      <p class="opt-inbox-detail-note">完整差异与验证记录在审核工作台中。</p>
      </div><footer class="opt-panel-footer">${actions(button("请求修改","quiet")+button("进入审核","primary","arrow"))}</footer></aside></section>`;
  }

  const renderers = {home,dashboard,conversations,projects,editor,inbox};
  window.OPT = {meta,theme,main,app,esc,icon,button,pill,toggle,actions,header,card,field,input,select,textarea,tabs,notice,empty,providerIcon,sectionHead,kv,row,settings,home,renderers,room};
})();
