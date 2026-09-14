(() => {
  const data=window.FINAL_DEMO,pages=data.pages;
  const byRef=ref=>{
    const value=decodeURIComponent(String(ref||"").replace(/^#/,""));
    return pages.find(page=>page.id===value||page.pageId===value.toUpperCase())||pages[0];
  };
  let current=byRef(location.hash),theme="dark",toastTimer;
  try{theme=localStorage.getItem("r-code.final-demo.theme")==="light"?"light":"dark";}catch{}
  const frame=document.querySelector("#demo-frame"),drawer=document.querySelector("#demo-drawer"),scrim=document.querySelector("#drawer-scrim"),search=document.querySelector("#demo-search"),list=document.querySelector("#demo-pages"),loading=document.querySelector("#demo-loading"),controller=document.querySelector("#demo-controller"),restore=document.querySelector("#restore-controller");
  const routeUrl=page=>`${page.render}?theme=${theme}&motion=1&demo=1`;
  function renderList(){
    const query=search.value.trim().toLowerCase();list.replaceChildren();let group="",count=0;
    for(const page of pages.filter(page=>[page.pageId,page.id,page.title,page.group].join(" ").toLowerCase().includes(query))){
      if(page.group!==group){const label=document.createElement("div");label.className="demo-group";label.textContent=page.group;list.append(label);group=page.group;}
      const button=document.createElement("button");button.type="button";button.className="demo-page";button.dataset.id=page.id;button.setAttribute("aria-current",String(page.id===current.id));
      const code=document.createElement("code");code.textContent=page.pageId;const name=document.createElement("span");name.textContent=page.title;const state=document.createElement("em");state.textContent=page.decision==="keep"?"保留":"已采用";
      button.append(code,name,state);button.onclick=()=>navigate(page);list.append(button);count++;
    }
    if(!count){const empty=document.createElement("p");empty.className="demo-empty";empty.textContent="没有匹配的版面";list.append(empty);}
  }
  function updateTheme(){
    document.documentElement.dataset.theme=theme;
    document.querySelectorAll("[data-theme]").forEach(button=>button.setAttribute("aria-pressed",String(button.dataset.theme===theme)));
    try{localStorage.setItem("r-code.final-demo.theme",theme);}catch{}
  }
  function load(){
    loading.classList.add("busy");frame.src=routeUrl(current);
    document.querySelector("#current-page-id").textContent=current.pageId;
    document.querySelector("#current-page-title").textContent=current.title;
    document.title=`${current.pageId} · ${current.title} · R-Code 最终 Demo`;
    history.replaceState(null,"",`#${current.pageId}`);renderList();
  }
  function navigate(page){current=typeof page==="string"?byRef(page):page;search.value="";closeDrawer();load();}
  function step(delta){const index=pages.findIndex(page=>page.id===current.id);navigate(pages[(index+delta+pages.length)%pages.length]);}
  function openDrawer(){drawer.classList.add("open");drawer.setAttribute("aria-hidden","false");scrim.hidden=false;document.querySelector("#open-drawer").setAttribute("aria-expanded","true");renderList();setTimeout(()=>search.focus(),50);}
  function closeDrawer(){drawer.classList.remove("open");drawer.setAttribute("aria-hidden","true");scrim.hidden=true;document.querySelector("#open-drawer").setAttribute("aria-expanded","false");}
  function showToast(message){clearTimeout(toastTimer);const toast=document.querySelector("#demo-toast");toast.textContent=message;toast.classList.add("show");toastTimer=setTimeout(()=>toast.classList.remove("show"),2600);}
  function setTheme(next){if(next===theme)return;theme=next;updateTheme();load();}
  frame.addEventListener("load",()=>loading.classList.remove("busy"));
  document.querySelector("#open-drawer").onclick=()=>drawer.classList.contains("open")?closeDrawer():openDrawer();
  document.querySelector("#close-drawer").onclick=closeDrawer;scrim.onclick=closeDrawer;
  document.querySelector("#previous-page").onclick=()=>step(-1);document.querySelector("#next-page").onclick=()=>step(1);
  document.querySelector("#quick-theme").onclick=()=>setTheme(theme==="dark"?"light":"dark");
  document.querySelectorAll("[data-theme]").forEach(button=>button.onclick=()=>setTheme(button.dataset.theme));
  document.querySelector("#hide-controller").onclick=()=>{controller.classList.add("is-hidden");restore.hidden=false;};
  restore.onclick=()=>{restore.hidden=true;controller.classList.remove("is-hidden");};
  search.oninput=renderList;
  window.addEventListener("hashchange",()=>{const page=byRef(location.hash);if(page.id!==current.id){current=page;load();}});
  window.addEventListener("message",event=>{
    const message=event.data;if(!message||message.source!=="r-code-final-demo")return;
    if(message.type==="navigate")navigate(message.page);
    if(message.type==="theme")setTheme(message.theme);
    if(message.type==="toast")showToast(message.message);
  });
  window.addEventListener("keydown",event=>{
    if(event.key==="Escape"&&drawer.classList.contains("open")){closeDrawer();return;}
    if(event.key.toLowerCase()==="h"&&!event.ctrlKey&&!event.metaKey&&document.activeElement?.tagName!=="INPUT"){
      const hidden=controller.classList.toggle("is-hidden");restore.hidden=!hidden;
    }
    if(event.altKey&&event.key==="ArrowLeft")step(-1);
    if(event.altKey&&event.key==="ArrowRight")step(1);
  });
  updateTheme();load();
})();
