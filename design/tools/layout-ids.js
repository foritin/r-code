(() => {
  const {pageId,title,regions=[]} = window.PROPOSAL;
  const stamp = document.createElement("div");
  stamp.className="opt-layout-stamp";
  const code=document.createElement("code");code.textContent=pageId;
  const label=document.createElement("span");label.textContent=title;
  stamp.append(code,label);
  if(document.body.dataset.motion==="on"){
    const hint=document.createElement("small");hint.textContent=matchMedia("(prefers-reduced-motion: reduce)").matches?"按系统偏好减少动态":"子代理名字流光";stamp.append(hint);
  }
  document.body.append(stamp);
  window.__getLayoutRegions = () => regions.flatMap(region => {
    const rects=[...document.querySelectorAll(region.selector)].map(el=>el.getBoundingClientRect()).filter(r=>r.width>0&&r.height>0);
    if(!rects.length)return [];
    const x=Math.min(...rects.map(r=>r.left)),y=Math.min(...rects.map(r=>r.top));
    const right=Math.max(...rects.map(r=>r.right)),bottom=Math.max(...rects.map(r=>r.bottom));
    return [{id:`${pageId}-${region.key}`,label:region.label,x,y,width:right-x,height:bottom-y,visible:right>0&&bottom>0&&x<innerWidth&&y<innerHeight}];
  });
  window.__LAYOUT_IDS_READY__=true;
})();
