(() => {
  const {meta,theme,main,esc,icon,button,pill,toggle,actions,card,field,input,select,textarea,tabs,notice,empty,providerIcon,sectionHead,kv,row,settings,home,renderers} = window.OPT;
  const providerRows = () => [
    ["deepseek","DeepSeek","deepseek-v4-pro",false,"环境变量",false],
    ["openai","OpenAI","gpt-5.6-sol",true,"环境变量",false],
    ["relay","relay-nokey","relay-alpha",false,"待完成",true],
  ].map(p=>`<div class="opt-row">${providerIcon(p[0])}<div class="opt-grow"><strong>${p[1]}　${p[3]?pill("默认","accent"):""}</strong><small class="opt-mono">${p[2]}</small></div>${pill(p[4],p[5]?"warning":"")}${button(p[5]?"补全配置":"编辑",p[5]?"":"quiet")}${button("⋯","quiet")}</div>`).join("");

  renderers["settings-providers"] = () => {
    const images = `<div class="opt-two"><div class="opt-card" style="border-color:var(--accent);background:var(--opt-tint)">${row("本机 OCR", "提取 PNG / JPEG 中的文字",pill("默认","accent"),"check")}<p class="opt-help">在本机完成，不调用模型服务。</p></div><div class="opt-card">${row("视觉模型","理解整张图片的内容与布局",pill("未选择"),"file")}<p class="opt-help">选择后配置服务与模型，使用其调用额度。</p></div></div>`;
    settings("模型服务","providers","管理对话使用的模型与凭据。",notice("<strong>1 项服务待完成配置</strong>　补全后可用于新对话。","warning",button("查看详情","quiet"))+card("对话模型",providerRows(),'<span class="opt-faint" style="font-size:12px">3 项服务</span>')+sectionHead("图片理解",button("使用说明","quiet"))+images+'<p class="opt-help">选择视觉模型后配置服务与模型。凭据由安全存储或环境变量提供。</p>',button("添加模型服务","primary","plus"));
  };

  renderers["settings-agents"] = () => {
    const settingLine = (title,note,control) => `<div class="opt-setting-line"><div><strong>${title}</strong><p>${note}</p></div><div class="opt-setting-control">${control}</div></div>`;
    const defaults = `<section class="opt-agent-choice opt-open-section"><header><h2>默认引擎</h2><span>用于新会话</span></header>${settingLine("主 Agent","运行中的任务继续使用原引擎。",select("R-Code · 自定义 Provider"))}</section>`;
    const runtime = `<section class="opt-runtime-section opt-open-section"><header><h2>Codex 运行时</h2><span class="opt-inline-state approval">未安装</span></header><div class="opt-runtime-row">${icon("code")}<div class="opt-runtime-copy"><strong>接入 Codex CLI</strong><p>安装后继续登录，再启用协作能力。</p><div class="opt-runtime-steps"><b>01 安装</b><span>›</span><span>02 登录</span><span>›</span><span>03 接入</span></div></div>${button("安装并继续","primary","arrow")}</div><div class="opt-runtime-detail"><span>路径与配置详情</span><span>展开　⌄</span></div></section>`;
    const routing = `<section class="opt-routing-section opt-open-section"><header><h2>委派路由</h2><span>按任务复杂度选择执行者</span></header>${settingLine("复杂度策略","优先按此策略安排新任务。",select("均衡 · 复杂任务优先 Codex"))}${settingLine("允许 Codex 子代理","不可用时回退到 R-Code，并显示原因。",toggle(true))}</section>`;
    const quality = `<section class="opt-quality-section opt-open-section"><header><h2>完成之后</h2></header>${settingLine("质量复核","运行后再次检查结果，会增加模型调用和等待时间。",toggle(false))}</section>`;
    settings("Agent 编排","agents","选择默认引擎，安排协作与结果复核。",defaults+runtime+routing+quality);
  };

  renderers["settings-subagents"] = () => {
    const sources = `<div class="opt-row">${providerIcon("deepseek")}<div class="opt-grow"><strong>DeepSeek API</strong><small>deepseek-v4-pro · 网络不可用</small></div>${pill("连接失败","danger")}${button("重试", "")}</div><div class="opt-row">${providerIcon("openai")}<div class="opt-grow"><strong>OpenAI API</strong><small>gpt-5.6-sol · 168 ms</small></div>${pill("已连通","success")}${button("测试", "quiet")}</div><div class="opt-row">${providerIcon("openai")}<div class="opt-grow"><strong>Codex CLI</strong><small>尚未安装运行时</small></div>${pill("未就绪")}${button("去配置", "quiet")}</div>`;
    settings("子代理配置","subagents","先确认来源状态，再配置候选槽位与权重。",'<div class="opt-check-steps"><strong>1 确认来源</strong><i></i><span>2 配置槽位</span><i></i><span>3 保存候选池</span></div>'+card("候选来源",sources,button("全部测试","quiet","refresh"))+card("候选槽位",empty("还没有候选槽位","添加来源与模型，再调整各槽位的权重。",button("添加槽位","primary","plus"),"layers")+'<div class="opt-footer"><span>0 / 3 槽位 · 当前权重 0%</span><span>配置后合计应为 100%</span></div>',pill("空候选池"))+notice("空候选池会保留原有委派路由，不会中断已经运行的子代理。")+'<div class="opt-footer"><span>完成槽位配置后可保存</span>'+button("保存候选池","primary","",true)+'</div>');
  };

  renderers["settings-tools"] = () => {
    const services = `<section class="opt-mcp-list"><header class="opt-mcp-section-head"><h2>已安装服务 <span>02</span></h2><span class="opt-inline-state approval">1 项等待审核</span></header><table class="opt-mcp-table"><colgroup><col style="width:47%"><col style="width:14%"><col style="width:17%"><col style="width:22%"></colgroup><thead><tr><th>服务</th><th>来源</th><th>状态</th><th class="right">操作</th></tr></thead><tbody>
      <tr><td><div class="opt-service-title">${icon("search")}<div><strong>项目搜索 MCP</strong><small>项目内的搜索服务，启动方案待审核。</small></div></div></td><td>R-Code 生成</td><td><span class="opt-inline-state approval">待审核</span></td><td><div class="opt-mcp-actions">${button("审核方案","quiet text-accent")}${button("⋯","quiet")}</div></td></tr>
      <tr><td><div class="opt-service-title">${icon("layers")}<div><strong>R-Code 深度调研</strong><small>内置多来源证据收集 · 3 个工具</small></div></div></td><td>内置</td><td><span class="opt-inline-state ready">按需就绪</span></td><td><div class="opt-mcp-actions">${toggle(true)}${button("⋯","quiet")}</div></td></tr>
      </tbody></table><p class="opt-mcp-note">外部服务通过启动审核后，才可按需启用。</p></section>`;
    const environment = `<section class="opt-environment-summary"><header><h2>本机工具</h2>${button("环境设置","quiet","arrow")}</header><div class="opt-environment-grid"><div>${icon("terminal")}<span><small>执行环境</small><strong>Git Bash</strong><em>已自动探测</em></span></div><div>${icon("search")}<span><small>联网工具</small><strong>网页搜索与读取</strong><em>可用</em></span></div><div>${icon("code")}<span><small>RTK 命令加速</small><strong>尚未安装</strong><em>启用后按规则安装</em></span></div></div></section>`;
    settings("工具与连接","tools","让需要的能力随任务就位。",tabs(["MCP 服务","环境与内置工具","扩展市场"],0,"line")+services+environment,button("添加 MCP 服务","primary","plus"));
  };

  function knowledgeTop(active) {
    return `<div class="opt-scope">${icon("folder")}<span>作用域</span><strong>r-code</strong>${pill("项目")}${button("切换作用域　⌄","quiet")}</div>`+tabs(["记忆","协作 Prompt","Skills"],active,"line");
  }
  renderers["settings-knowledge"] = () => {
    settings("知识与指令","knowledge","在全局规则之上，管理当前项目的长期上下文。",knowledgeTop(0)+notice('<strong>旧版记忆文件可能进入 Git 历史</strong><br>已发现的历史风险应先检查，再决定是否继续使用。',"warning",button("查看详情","quiet"))+card("记忆已关闭",'<p class="opt-help">当前不会读取或自动复盘会话。项目模式已配置为读写，但在全局记忆启用前不会生效。</p>'+kv([["项目模式","读写 · 暂未生效"],["继承关系","启用后继承全局记忆"]])+actions(button("启用全局记忆","primary")),pill("关闭"))+empty("暂无项目记忆","启用记忆后，可以添加稳定事实，或复盘成功的对话。",button("添加记忆","","plus",true),"layers"));
  };
  renderers["settings-prompts"] = () => {
    settings("知识与指令","knowledge","设置项目协作规则，并保留清楚的继承关系。",knowledgeTop(1)+`<div class="opt-card" style="padding:16px 20px"><div class="opt-card-head" style="margin-bottom:6px"><h2 style="font-size:14px">项目规则的应用方式</h2>${tabs(["追加到全局规则后","覆盖全局规则"])}</div><p class="opt-help" style="margin:0">先应用全局，再应用项目规则。当前未添加项目规则，仍继承全局。</p></div>`+field("主 Agent 规则",textarea("","输入仅针对当前项目的主 Agent 规则","style=\"height:136px\""),"说明委派边界、汇总方式和最终责任。")+field("子代理规则",textarea("","输入仅针对当前项目的子代理规则","style=\"height:120px\""),"约束任务范围、输出形式与验证责任。")+'<div class="opt-footer"><span>尚无项目规则 · 空白内容继续继承全局</span>'+actions(button("移除项目规则","quiet","",true)+button("保存并应用","primary","",true))+'</div>');
  };
  renderers["settings-skills"] = () => {
    const skills = [["mcp-creator","创建 MCP 服务草稿"],["skill-creator","创建和注册自定义 Skill"],["review-changes","审核并接受任务变更"],["git-commit-push","提交并推送已接受的变更"]];
    const detail = `${pill("继承自全局")}${pill("已启用","success")}<h2>/mcp-creator</h2><p>创建 MCP 服务源码并保存为待用户审核的禁用草稿。</p>${sectionHead("指令")}${'<div class="opt-readonly">帮用户创建全局 MCP 草稿：只声明凭据变量名，不填值；不得启动或启用服务；验证后调用 <span class="opt-mono">mcp_create_draft</span>。</div>'}${row("在 / 补全中启用","由全局设置控制。",pill("已启用","success"))}<div class="opt-footer"><span>此处为继承内容，只读</span>${button("在全局查看","","arrow")}</div>`;
    settings("知识与指令","knowledge","查看继承能力，管理当前项目的专属 Skills。",knowledgeTop(2)+tabs(["继承自全局 · 4","项目专属 · 0"])+`<div class="opt-skill-layout"><aside class="opt-skill-list">${skills.map((s,i)=>`<div class="${i===0?"selected":""}"><strong>/${s[0]}</strong><small>${s[1]}</small></div>`).join("")}</aside><article class="opt-skill-detail">${detail}</article></div>`,button("新建项目 Skill","primary","plus"));
  };

  renderers["settings-permissions"] = () => {
    settings("权限","permissions","查看 Codex 子代理当前使用的权限模式。",card("当前模式",row("仅查看","Codex 子代理的当前配置状态。",pill("只读状态"),"shield")+kv([["适用对象","Codex 子代理"],["配置来源","Codex 运行时设置"]])+notice("权限配置在 Codex 运行时中统一管理。这里显示当前生效值。")+actions(button("管理 Codex 权限","primary","arrow")))+sectionHead("模式说明")+'<div class="opt-readonly">仅查看 · 请求批准 · 替我审批 · 完全访问 · 自定义<br><span class="opt-muted">查看各模式差异与配置来源　⌄</span></div>');
  };
  renderers["settings-security"] = () => {
    settings("隐私与安全","security","这些保护由应用统一管理，此处仅展示状态。",card("内置保护",row("凭据存储","凭据由系统钥匙串或受保护配置管理。",pill("强制启用","success"),"lock")+row("日志与支持包脱敏","导出前清理密钥、令牌和受保护内容。",pill("强制启用","success"),"shield")+row("内容与沙箱策略","应用统一应用内容安全策略及沙箱边界。",pill("强制启用","success"),"code"))+notice("安全策略由应用统一管理，可展开查看具体的保护范围。")+'<div class="opt-footer"><span>技术说明与保护范围</span><span>展开查看　⌄</span></div>');
  };

  renderers["settings-appearance"] = () => {
    const previews = [["light","亮色","清晰明快"],["dark","暗色","沉浸专注"],["system","跟随系统","自动切换"]].map((t,i)=>`<button class="opt-theme-choice ${((theme==="obsidian"&&i===1)||(theme==="studio-light"&&i===0))?"selected":""}"><div class="opt-theme-window ${t[0]}"><i></i><aside><b></b><b></b><b></b></aside><main><b></b><em></em></main></div><strong>${t[1]}</strong><small class="opt-faint" style="font-size:11px">${t[2]}</small></button>`).join("");
    settings("外观与小助手","appearance","选择适合当前环境的显示方式。",row("界面语言","更改后立即生效。",'<div style="width:230px">'+select("简体中文")+'</div>')+sectionHead("界面主题")+`<div class="opt-theme-grid">${previews}</div>`+sectionHead("R-Code 初音小助手",toggle(true))+card("",row("默认形态","完整形态更易看清，迷你形态减少遮挡。",tabs(["完整形态","迷你形态"]))+row("悬浮位置","角色可在桌面拖动。",button("恢复右下角","quiet","refresh"))+row("动效","遵循系统减少运动偏好。",'<div style="width:200px">'+select("跟随系统")+'</div>')+row("状态提示音","仅在授权、完成、失败或待审核时提示。",toggle(false)))+'<p class="opt-help">外观偏好即时生效，不会清空正在编辑的会话草稿。</p>');
  };

  renderers["settings-lifecycle"] = () => {
    settings("启动与关闭","lifecycle","设置关闭窗口时的行为。",notice('<strong>窗口设置暂时不可用</strong><br>当前演示环境未连接桌面窗口服务，尚未读取到已保存的设置。',"warning",button("重新检测","","refresh"))+card("点击窗口关闭按钮时",'<div class="opt-input" style="color:var(--fg-faint)">读取设置后可修改</div><p class="opt-help">连接桌面服务后，可以选择每次询问、后台运行或退出应用。</p>')+'<div class="opt-footer"><span>技术详情</span><span class="opt-mono">cmd_close_behavior_get　⌄</span></div>'+sectionHead("其他操作")+row("退出应用","请在桌面 App 中执行；运行中的任务需先妥善收尾。",button("退出应用","danger","",true)));
  };
  renderers["settings-updates"] = () => {
    settings("更新","updates","检查 R-Code 正式版本，按需完成下载与安装。",'<section class="opt-card" style="max-width:760px;padding:30px"><div class="opt-row" style="padding-top:0"><span class="opt-provider-icon" style="width:52px;height:52px;font-size:24px;color:var(--accent)">R</span><div class="opt-grow"><strong style="font-size:23px">R-Code</strong><small>当前版本 1.0.0</small></div>'+button("检查更新","primary","refresh")+'</div>'+kv([["更新状态",pill("等待检查")],["上次检查","尚未检查"]])+'<div class="opt-footer"><span>下载和安装始终需要你的明确操作</span></div></section>'+notice("自动检查最多每 6 小时请求一次版本信息；不会自动下载安装。")+'<p class="opt-help">点击检查更新，获取正式版本信息。</p>');
  };
  renderers["settings-diagnostics"] = () => {
    const logs = '<div class="opt-toolbar" style="padding-top:0">'+tabs(["全部","error","warn","info","debug"])+actions('<span class="opt-faint" style="font-size:11px">记录级别</span><select class="opt-input" style="width:92px;min-height:30px;padding:5px 9px"><option>info</option></select>')+'</div><pre class="opt-diff" style="height:300px;font-size:12px;line-height:2">20:54  <span style="color:var(--accent)">INFO </span>  r_code::demo     完整浏览器 Demo 已就绪\n20:54  <span class="opt-faint">DEBUG</span>  r_code::gateway  所有数据均保存在当前页面内存中</pre>';
    settings("诊断","diagnostics","查看日志，或准备可分享的脱敏支持信息。",card("诊断日志",logs,'<span class="opt-faint" style="font-size:12px">最近 7 天</span>')+notice("支持信息先预览，确认内容后再导出。预览本身不会写入文件。", "", button("预览支持信息","primary","file"))+row("高级请求审计","需要核对模型请求构成时再启用。",toggle(false))+'<div class="opt-help">筛选只改变当前日志视图；记录级别影响后续记录。请求审计详细选项可继续展开。</div>',button("刷新日志","","refresh"));
  };

  function providerEditor() {
    renderers["settings-providers"]();
    document.querySelectorAll(".drawer-panel,.drawer-backdrop").forEach(el=>el.remove());
    const html = `<div class="opt-drawer-backdrop"></div><aside class="opt-drawer"><header class="opt-panel-head">${providerIcon("deepseek")}<div style="flex:1"><h2>DeepSeek</h2><p class="opt-help" style="margin:3px 0 0">编辑模型服务</p></div>${button("×","quiet")}</header><div class="opt-panel-body">${field("配置名称",input("deepseek","","readonly"),"配置名称作为现有服务标识保留。")}${field("模型",select("deepseek-v4-pro"),"服务返回 3 个可用模型。")}${field("访问密钥",input("","留空则保留当前配置","type=\"password\""),'<span style="color:var(--opt-success)">由环境变量提供</span> · 不回显已保存的密钥。')}${row("显示思考过程","展示模型明确返回的思考内容或摘要。",toggle(true))}${row("设为新会话默认","当前默认仍为 OpenAI。",toggle(false))}${notice("普通保存只更新此服务；设为默认需明确选择。")}${sectionHead("高级设置",'<span class="opt-faint">⌄</span>')}<div class="opt-help">OpenAI Chat Completions · 输出上限 393216<br>路径、温度和联网线路保持现有配置入口。</div></div><footer class="opt-panel-footer"><div class="opt-footer" style="margin:0;padding:0;border:0">${button("删除服务","danger")}${actions(button("取消")+button("保存更改","primary"))}</div></footer></aside>`;
    document.body.insertAdjacentHTML("beforeend",html);
    if(meta.id==="provider-discard")document.body.insertAdjacentHTML("beforeend",'<div class="opt-confirm-backdrop"></div><section class="opt-confirm">'+icon("info")+'<h2 style="margin-top:15px">放弃未保存的更改？</h2><p>DeepSeek 的本次编辑尚未保存。放弃后将恢复上次保存的配置。</p>'+actions(button("继续编辑")+button("放弃更改","danger"))+'</section>');
  }
  renderers["provider-editor"] = providerEditor;
  renderers["provider-discard"] = providerEditor;

  function search() {
    home();
    document.querySelectorAll(".ovl-backdrop").forEach(el=>el.remove());
    const noResults=meta.id==="search-empty";
    const content=noResults?`<h3>未找到匹配内容</h3><p>在 r-code 中没有找到“没有这个演示条目”。<br>试试文件名、路径或文件中的关键词。</p>${actions(button("清空搜索","primary"))}`:`<span class="opt-eyebrow">文件示例</span><div class="opt-search-result">${icon("file")}Cargo.toml<small>项目根目录</small></div><div class="opt-search-result">${icon("file")}README.md<small>项目说明</small></div><div class="opt-search-result">${icon("folder")}src/<small>源文件目录</small></div>`;
    document.body.insertAdjacentHTML("beforeend",`<div class="opt-search-backdrop"></div><section class="opt-search-modal"><div class="opt-search-scope"><span>当前搜索范围　<strong style="color:var(--fg)">r-code</strong></span><span>D:/project/rust/r-code</span></div><div class="opt-search-input">${icon("search")}<input value="${noResults?"没有这个演示条目":""}" placeholder="搜索文件名、路径或内容…">${button("×","quiet")}</div><div class="opt-search-results">${content}</div><footer class="opt-search-footer"><span>↑ ↓ 选择</span><span>Enter 打开</span><span>Esc 关闭</span><span>仅当前附加文件夹</span></footer></section>`);
  }
  renderers.search=search;renderers["search-empty"]=search;

  // These pages passed review unchanged. Keeping the captured implementation is the final decision.
  for(const id of ["activity","archive","settings-notifications"])renderers[id]=()=>{};

  const roomIds=["conversation","tool-launcher","runs","files","terminal","review","plan","permission"];
  if(roomIds.includes(meta.id))window.OPT.room(meta.id);
  else if(renderers[meta.id])renderers[meta.id]();
  else throw new Error(`Missing proposal renderer: ${meta.id}`);
  document.querySelectorAll("#app input[type=hidden],#app .toast-stack").forEach(el=>el.remove());
  window.__PROPOSAL_READY__=true;
})();
