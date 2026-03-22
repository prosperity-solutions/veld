"use strict";(()=>{var so=Object.defineProperty;var B=(t,o)=>()=>(t&&(o=t(t=0)),o);var lt=(t,o)=>{for(var n in o)so(t,n,{get:o[n],enumerable:!0})};function ct(t,o){e={threads:[],lastEventSeq:0,lastSeenAt:{},agentListening:!1,panelOpen:!1,panelTab:"active",activePopover:null,activeMode:null,hoveredEl:null,lockedEl:null,toolbarOpen:!1,hidden:!1,shortcutsDisabled:!1,theme:"auto",expandedThreadId:null,pins:{},captureStream:null,drawLoaded:!1,drawCleanup:null,drawCanvas:null,prevOverflow:null,shadow:t,hostEl:o,toolbarContainer:null,fab:null,fabBadge:null,toolbar:null,toolBtnSelect:null,toolBtnScreenshot:null,toolBtnDraw:null,toolBtnPageComment:null,toolBtnComments:null,toolBtnHide:null,listeningModule:null,overlay:null,hoverOutline:null,componentTraceEl:null,screenshotRect:null,panel:null,panelBody:null,panelHeadTitle:null,panelBackBtn:null,markReadBtn:null,segBtnActive:null,segBtnResolved:null,tooltip:null,fabCX:0,fabCY:0,rafPending:!1,lastPathname:window.location.pathname}}var e,g=B(()=>{"use strict"});var re,s,pe,F,R,V,H,b,x=B(()=>{"use strict";re="/__veld__/feedback/api",s="veld-feedback-",pe=/Mac|iPhone|iPad/.test(navigator.platform),F=pe?"\u2318":"Ctrl",R=pe?"\u21E7":"Shift",V=" "+F+"\u21B5",H=16,b={logo:'<svg viewBox="0 0 32 32" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M13.2 28L4 4H8.4L15.7 23.8H15.8L23.1 4H27.5L18.3 28H13.2Z" fill="currentColor"/><path d="M24.5 29C25.88 29 27 27.88 27 26.5C27 25.12 25.88 24 24.5 24C23.12 24 22 25.12 22 26.5C22 27.88 23.12 29 24.5 29Z" fill="#C4F56A"/></svg>',crosshair:'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="2" x2="12" y2="6"/><line x1="12" y1="18" x2="12" y2="22"/><line x1="2" y1="12" x2="6" y2="12"/><line x1="18" y1="12" x2="22" y2="12"/></svg>',chat:'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M21 15a2 2 0 01-2 2H7l-4 4V5a2 2 0 012-2h14a2 2 0 012 2z"/></svg>',pageComment:'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="8" y1="13" x2="16" y2="13"/><line x1="8" y1="17" x2="12" y2="17"/></svg>',send:'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="22" y1="2" x2="11" y2="13"/><polygon points="22 2 15 22 11 13 2 9 22 2"/></svg>',screenshot:'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><rect x="3" y="3" width="18" height="18" rx="2"/><path d="M8 3v4M16 3v4M3 8h4M3 16h4M17 8h4M17 16h4M8 17v4M16 17v4"/></svg>',check:'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>',eyeOff:'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M17.94 17.94A10.07 10.07 0 0112 20c-7 0-11-8-11-8a18.45 18.45 0 015.06-5.94M9.9 4.24A9.12 9.12 0 0112 4c7 0 11 8 11 8a18.5 18.5 0 01-2.16 3.19m-6.72-1.07a3 3 0 11-4.24-4.24"/><line x1="1" y1="1" x2="23" y2="23"/></svg>',cancel:'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>',robot:'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><rect x="3" y="4" width="18" height="14" rx="2"/><circle cx="9" cy="11" r="1.5" fill="currentColor" stroke="none"/><circle cx="15" cy="11" r="1.5" fill="currentColor" stroke="none"/><path d="M12 1v3M8 21h8"/></svg>',resolve:'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><polyline points="16 9 10.5 15 8 12.5"/></svg>',draw:'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9"/><path d="M16.5 3.5a2.121 2.121 0 013 3L7 19l-4 1 1-4L16.5 3.5z"/></svg>',keyboard:'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="4" width="20" height="16" rx="2"/><path d="M6 8h.01M10 8h.01M14 8h.01M18 8h.01M6 12h.01M10 12h.01M14 12h.01M18 12h.01M8 16h8"/></svg>',back:'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="15 18 9 12 15 6"/></svg>',copy:'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 01-2-2V4a2 2 0 012-2h9a2 2 0 012 2v1"/></svg>',person:'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 21v-2a4 4 0 00-4-4H8a4 4 0 00-4 4v2"/><circle cx="12" cy="7" r="4"/></svg>',dashboard:'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 9l9-7 9 7v11a2 2 0 01-2 2H5a2 2 0 01-2-2z"/><polyline points="9 22 9 12 15 12 15 22"/></svg>'}});function Le(t){return pe?t.metaKey:t.ctrlKey}function pt(t){if(t.id)return"#"+CSS.escape(t.id);let o=[],n=t;for(;n&&n!==document.body&&n!==document.documentElement;){let a=n.tagName.toLowerCase();if(n.id){o.unshift("#"+CSS.escape(n.id));break}if(n.className&&typeof n.className=="string"){let d=n.className.trim().split(/\s+/).filter(l=>l&&!l.startsWith(s));d.length&&(a+="."+d.map(CSS.escape).join("."))}let r=n.parentElement;r&&Array.from(r.children).filter(l=>l.tagName===n.tagName).length>1&&(a+=":nth-child("+(Array.from(r.children).indexOf(n)+1)+")"),o.unshift(a),n=r}return o.join(" > ")}function i(t,o,n){let a=document.createElement(t);return o&&(a.className=o.split(" ").map(r=>s+r).join(" ")),n!==void 0&&(a.textContent=n),a}function fe(t){let o=t.getBoundingClientRect();return{x:o.left+window.scrollX,y:o.top+window.scrollY,width:o.width,height:o.height}}function $(t,o){t.addEventListener("keydown",function(n){n.key==="Enter"&&Le(n)&&(n.preventDefault(),o.click())})}function ve(t){let o=Date.now(),n=new Date(t).getTime(),a=Math.floor((o-n)/1e3);if(a<60)return a+"s ago";let r=Math.floor(a/60);if(r<60)return r+"m ago";let d=Math.floor(r/60);return d<24?d+"h ago":Math.floor(d/24)+"d ago"}function z(t,o){if(!t.messages.length)return!1;let n=o[t.id]||0;for(let a=0;a<t.messages.length;a++)if(t.messages[a].author.type==="agent"&&new Date(t.messages[a].created_at).getTime()>n)return!0;return!1}function he(t){return window.location.pathname===t}function U(t){return t.scope.page_url||"/"}function ft(t){return t.scope.position||null}function S(t,o){return t.find(n=>n.id===o)}function ue(t){if(!t||!t.length)return null;let o=[t[0]];for(let n=1;n<t.length;n++)t[n]!==t[n-1]&&o.push(t[n]);return o.length>5&&(o=o.slice(o.length-5)),o.join(" > ")}var k=B(()=>{"use strict";x()});function ht(){e.tooltip=i("div","tooltip"),e.shadow.appendChild(e.tooltip)}function po(t,o){e.tooltip.innerHTML=o,e.tooltip.style.display="block";let n=t.getBoundingClientRect(),a=e.tooltip.offsetWidth,r=e.tooltip.offsetHeight,d=8,l=n.top+window.scrollY-r-d;l<window.scrollY+4&&(l=n.bottom+window.scrollY+d);let c=n.left+window.scrollX+n.width/2-a/2;c=Math.max(window.scrollX+4,Math.min(window.scrollX+window.innerWidth-a-4,c)),e.tooltip.style.top=l+"px",e.tooltip.style.left=c+"px"}function vt(){e.tooltip.style.display="none"}function _(t,o){let n=t;if(o&&o.length){n+=' <span class="'+s+'kbd-group">';for(let a=0;a<o.length;a++)n+='<kbd class="'+s+'kbd">'+o[a]+"</kbd>";n+="</span>"}return n}function N(t,o){t.addEventListener("mouseenter",()=>{po(t,o)}),t.addEventListener("mouseleave",vt),t.addEventListener("mousedown",vt)}function ut(t,o){let r=o.top+window.scrollY-t.offsetHeight-6,d=o.top+window.scrollY+o.height+6;r<window.scrollY+8?t.style.top=d+"px":t.style.top=r+"px";let l=o.left+window.scrollX,c=window.scrollX+window.innerWidth-t.offsetWidth-8;t.style.left=Math.max(window.scrollX+8,Math.min(c,l))+"px"}var me=B(()=>{"use strict";g();k();x()});function m(t,o){let n=i("div","toast",t);o&&(n.style.background="#dc2626"),e.shadow.appendChild(n),requestAnimationFrame(function(){n.classList.add(s+"toast-show")}),setTimeout(function(){n.classList.remove(s+"toast-show"),setTimeout(function(){n.remove()},300)},2800)}var X=B(()=>{"use strict";g();k();x()});function Lt(t){Y=t.setMode,Et=t.togglePageComment,Mt=t.togglePanel,Pt=t.hideOverlay}function j(t,o,n){let a=i("button","tool-btn");return a.dataset.action=t,a.innerHTML=o,N(a,n),a.addEventListener("click",r=>{r.stopPropagation(),uo(t)}),a}function uo(t){console.log("[veld] handleToolAction:",t,"setModeFn:",typeof Y),t==="select-element"?Y(e.activeMode==="select-element"?null:"select-element"):t==="screenshot"?Y(e.activeMode==="screenshot"?null:"screenshot"):t==="draw"?Y(e.activeMode==="draw"?null:"draw"):t==="page-comment"?Et():t==="show-comments"?Mt():t==="hide"&&Pt()}function J(){e.toolbarOpen=!e.toolbarOpen,e.toolbar.classList.toggle(s+"toolbar-open",e.toolbarOpen),!e.toolbarOpen&&Y&&Y(null)}var Y,Et,Mt,Pt,be=B(()=>{"use strict";g();k();x();me()});function y(t,o,n){let a={method:t,headers:{"Content-Type":"application/json"}};return n!==void 0&&(a.body=JSON.stringify(n)),fetch(re+o,a).then(r=>{if(!r.ok)throw new Error("API "+t+" "+o+" failed: "+r.status);return r.status===204?null:r.json()})}var Z=B(()=>{"use strict";x()});function $t(t){Ne=t.addPin,Xe=t.updateBadge,Ue=t.renderPanel}function Ye(t,o){let d=o.y+o.height+10,l=o.y-260-10,c;d+260>window.scrollY+window.innerHeight-16&&l>window.scrollY+16?c=l:c=d;let p=o.x+o.width/2-180,f=window.scrollX+window.innerWidth-360-16,v=window.scrollX+16;p=Math.max(v,Math.min(f,p)),t.style.top=c+"px",t.style.left=p+"px"}function M(){e.activePopover&&(typeof e.activePopover._veldCleanup=="function"&&e.activePopover._veldCleanup(),e.activePopover.remove(),e.activePopover=null),e.lockedEl&&(e.lockedEl=null,e.hoverOutline.style.display="none",e.componentTraceEl.style.display="none"),e.toolBtnPageComment&&e.toolBtnPageComment.classList.remove(s+"tool-active"),e.toolBtnScreenshot&&e.toolBtnScreenshot.classList.remove(s+"tool-active")}function je(t,o,n,a,r){M(),e.lockedEl=a;let d=i("div","popover");if(o&&d.appendChild(i("div","popover-selector",o)),r){let u=ue(r);u&&d.appendChild(i("div","popover-trace",u))}let l=i("div","popover-body"),c=document.createElement("textarea");c.className=s+"textarea",c.placeholder="Leave feedback...",c.rows=3,l.appendChild(c);let p=i("div","popover-actions"),f=i("button","btn btn-secondary btn-sm","Cancel");f.addEventListener("click",M),p.appendChild(f);let v=i("button","btn btn-primary btn-sm","Send"+V);v.addEventListener("click",()=>{let u=c.value.trim();if(!u||v.disabled)return;v.disabled=!0;let h=o?{type:"element",page_url:window.location.pathname,selector:o,position:t}:{type:"page",page_url:window.location.pathname};y("POST","/threads",{scope:h,message:u,component_trace:r||null,viewport_width:window.innerWidth,viewport_height:window.innerHeight}).then(w=>{e.threads.push(w),M(),Ne&&Ne(w),Xe&&Xe(),e.panelOpen&&Ue&&Ue(),m("Thread created")}).catch(()=>{v.disabled=!1,m("Failed to create thread",!0)})}),p.appendChild(v),$(c,v),l.appendChild(p),d.appendChild(l),e.shadow.appendChild(d),e.activePopover=d,Ye(d,t),c.focus()}function qe(){if(e.activePopover){M();return}je({x:window.innerWidth/2-180+window.scrollX,y:120+window.scrollY,width:0,height:0},null,null,null,null),e.toolBtnPageComment.classList.add(s+"tool-active")}var Ne,Xe,Ue,ye=B(()=>{"use strict";g();k();x();Z();X()});var Zt={};lt(Zt,{ensureDrawScript:()=>Ge,setDrawModeDeps:()=>Ke,setupGlobalDrawCanvas:()=>We,teardownGlobalDrawCanvas:()=>Ve});function Ke(t){Jt=t.setMode}function Ge(){return e.drawLoaded&&window.__veld_draw?Promise.resolve():new Promise((t,o)=>{let n=document.createElement("script");n.src="/__veld__/feedback/draw.js",n.onload=()=>{e.drawLoaded=!0,t()},n.onerror=o,(document.head||document.documentElement).appendChild(n)})}function We(){e.drawCanvas=document.createElement("canvas"),e.drawCanvas.className=s+"draw-canvas",document.body.appendChild(e.drawCanvas),e.prevOverflow=document.body.style.overflow,document.body.style.overflow="hidden";let t=e.captureStream&&e.captureStream.getVideoTracks()[0],o=t&&typeof ImageCapture<"u"?new ImageCapture(t):null;(o?o.grabFrame().catch(()=>null):Promise.resolve(null)).then(a=>{e.drawCanvas&&(e.drawCleanup=window.__veld_draw.activate(e.drawCanvas,{pageSnapshot:a,mountTarget:e.shadow,onDone:r=>{if(r){let d=e.drawCanvas,l=e.drawCleanup;e.drawCleanup=null,l&&l(),d&&d.parentNode&&d.parentNode.removeChild(d),e.drawCanvas=null,e.prevOverflow!==null&&(document.body.style.overflow=e.prevOverflow,e.prevOverflow=null),e.activeMode=null,e.toolBtnDraw.classList.remove(s+"tool-active");let c="[class^='"+s+"'], [class*=' "+s+"']",p=Array.from(document.querySelectorAll(c)).concat(Array.from(e.shadow.querySelectorAll(c)));e.hostEl.style.visibility="hidden";let f=[];p.forEach(h=>{h.style.display!=="none"&&(f.push({el:h,prev:h.style.visibility}),h.style.visibility="hidden")});let v=e.captureStream,u=v&&v.getVideoTracks()[0];u&&typeof ImageCapture<"u"?requestAnimationFrame(()=>{requestAnimationFrame(()=>{setTimeout(()=>{new ImageCapture(u).grabFrame().then(w=>{G(f),K();let C=document.createElement("canvas");C.width=w.width,C.height=w.height;let ce=C.getContext("2d");ce.drawImage(w,0,0),ce.drawImage(d,0,0,C.width,C.height),w.close(),C.toBlob(P=>{P&&de(P,0,0,window.innerWidth,window.innerHeight)},"image/png")}).catch(()=>{G(f),K(),d.toBlob(w=>{w&&de(w,0,0,window.innerWidth,window.innerHeight)},"image/png")})},50)})}):(G(f),K(),d.toBlob(h=>{h&&de(h,0,0,window.innerWidth,window.innerHeight)},"image/png"))}else Jt(null)}}))})}function Ve(){e.drawCleanup&&(e.drawCleanup(),e.drawCleanup=null),e.drawCanvas&&e.drawCanvas.parentNode&&e.drawCanvas.parentNode.removeChild(e.drawCanvas),e.drawCanvas=null,e.prevOverflow!==null&&(document.body.style.overflow=e.prevOverflow,e.prevOverflow=null)}var Jt,we=B(()=>{"use strict";g();x();ke()});function eo(t){Qt=t.setMode}function $e(){if(e.captureStream)return Promise.resolve();let t="veld-screenshot-disclaimer-seen",o=!1;try{o=sessionStorage.getItem(t)==="1"}catch{}return(o?Promise.resolve():new Promise((a,r)=>{let d=i("div","confirm-backdrop"),l=i("div","confirm-modal"),c=i("div","confirm-message");c.style.fontWeight="600",c.style.fontSize="14px",c.style.marginBottom="8px",c.textContent="Quick heads-up!",l.appendChild(c);let p=i("div","confirm-message");p.innerHTML="Your browser is about to ask you to share this tab. Don\u2019t worry \u2014 <strong>no one is calling you on Teams.</strong> Veld just needs to peek at your tab to capture pixel-perfect screenshots. Nothing leaves your machine, pinky promise.<br><br>You\u2019ll see a \u201CSharing this tab\u201D banner \u2014 that\u2019s normal! It stays while screenshot mode is active and goes away when you\u2019re done.",l.appendChild(p);let f=i("div","confirm-actions"),v=i("button","btn btn-secondary","Nah, skip it");v.addEventListener("click",()=>{d.remove(),r()});let u=i("button","btn btn-primary","Got it, let\u2019s go!");u.addEventListener("click",()=>{try{sessionStorage.setItem(t,"1")}catch{}d.remove(),a()}),f.appendChild(v),f.appendChild(u),l.appendChild(f),d.appendChild(l),e.shadow.appendChild(d),requestAnimationFrame(()=>{d.classList.add(s+"confirm-backdrop-visible")})})).then(()=>{let a={video:{displaySurface:"browser"},preferCurrentTab:!0};return navigator.mediaDevices.getDisplayMedia(a).then(r=>{e.captureStream=r,r.getVideoTracks()[0].addEventListener("ended",()=>{e.captureStream=null,e.activeMode==="screenshot"&&Promise.resolve().then(()=>(Je(),oo)).then(d=>d.setMode(null))})})})}function K(){e.captureStream&&(e.captureStream.getTracks().forEach(t=>{t.stop()}),e.captureStream=null)}function to(t,o,n,a){let r="[class^='"+s+"'], [class*=' "+s+"']",d=Array.from(document.querySelectorAll(r)).concat(Array.from(e.shadow.querySelectorAll(r)));e.hostEl.style.visibility="hidden";let l=[];d.forEach(v=>{v.style.display!=="none"&&(l.push({el:v,prev:v.style.visibility}),v.style.visibility="hidden")});let c=e.captureStream;if(e.captureStream=null,Qt(null),e.captureStream=c,!c){G(l),le(null,null,t,o,n,a);return}let p=c.getVideoTracks()[0];function f(){new ImageCapture(p).grabFrame().then(u=>{G(l),So(u,t,o,n,a)}).catch(()=>{G(l),le(null,null,t,o,n,a)})}requestAnimationFrame(()=>{requestAnimationFrame(()=>{setTimeout(f,50)})})}function G(t){t.forEach(o=>{o.el.style.visibility=o.prev}),e.hostEl.style.visibility=""}function So(t,o,n,a,r){let d=t.width/window.innerWidth,l=t.height/window.innerHeight,c=document.createElement("canvas");c.width=Math.round(a*d),c.height=Math.round(r*l),c.getContext("2d").drawImage(t,Math.round(o*d),Math.round(n*l),c.width,c.height,0,0,c.width,c.height),t.close(),c.toBlob(f=>{if(!f){le(null,null,o,n,a,r);return}de(f,o,n,a,r)},"image/png")}function de(t,o,n,a,r){let d="ss_"+Date.now()+"_"+Math.random().toString(36).slice(2,8);fetch(re+"/screenshots/"+d,{method:"POST",headers:{"Content-Type":"image/png"},body:t}).then(l=>{if(!l.ok)throw new Error("Upload failed: "+l.status);le(t,d,o,n,a,r)}).catch(l=>{m("Screenshot upload failed: "+l.message,!0),le(null,null,o,n,a,r)})}function le(t,o,n,a,r,d){M();let l=i("div","popover popover-screenshot");l._veldType="screenshot";let c=null,p=null;if(t){c=URL.createObjectURL(t);let P=i("div","screenshot-preview"),L=document.createElement("img");L.src=c,L.className=s+"screenshot-img",P.appendChild(L);let A=document.createElement("button");A.className=s+"annotate-btn",A.innerHTML=b.draw+" Annotate",A.type="button",A.addEventListener("click",()=>{Promise.resolve().then(()=>(we(),Zt)).then(({ensureDrawScript:ne})=>{ne().then(()=>{let O=document.createElement("canvas");O.className=s+"draw-canvas-inline";let Me=()=>{O.width=L.naturalWidth||L.width,O.height=L.naturalHeight||L.height};L.complete||L.addEventListener("load",Me,{once:!0}),Me(),P.appendChild(O),A.style.display="none";let D=document.createElement("button");D.className=s+"annotate-btn",D.innerHTML=b.check+" Done",D.type="button",P.appendChild(D),p=window.__veld_draw.activate(O,{inline:!0,baseImage:L,mountTarget:P,onDone:dt});function dt(){window.__veld_draw.compositeOnto(t,O).then(ae=>{t=ae,c&&URL.revokeObjectURL(c),c=URL.createObjectURL(ae),L.src=c,o&&fetch(re+"/screenshots/"+o,{method:"POST",headers:{"Content-Type":"image/png"},body:ae}).catch(()=>{m("Failed to upload annotated screenshot",!0)}),p&&(p(),p=null),O.parentNode&&O.parentNode.removeChild(O),D.parentNode&&D.parentNode.removeChild(D),A.style.display=""}).catch(ae=>{m("Annotation failed: "+ae.message,!0)})}D.addEventListener("click",dt)}).catch(()=>{m("Failed to load draw module",!0)})})}),P.appendChild(A),l.appendChild(P)}l._veldCleanup=()=>{p&&(p(),p=null),c&&(URL.revokeObjectURL(c),c=null)};let f=i("div","popover-header","Screenshot \u2014 "+window.location.pathname);l.appendChild(f);let v=i("div","popover-body"),u=i("textarea","textarea");u.placeholder="Describe what you see\u2026",v.appendChild(u);let h=i("div","popover-actions"),w=i("button","btn btn-secondary","Cancel");w.addEventListener("click",()=>{M()});let C=i("button","btn btn-primary","Send"+V);C.addEventListener("click",()=>{let P=u.value.trim();if(!P){u.focus();return}if(C.disabled)return;C.disabled=!0;let A={scope:{type:"page",page_url:window.location.pathname},message:P,component_trace:null,screenshot:o||null,viewport_width:window.innerWidth,viewport_height:window.innerHeight};y("POST","/threads",A).then(ne=>{e.threads.push(ne),M(),m("Thread created")}).catch(ne=>{C.disabled=!1,m("Failed to create thread: "+ne.message,!0)})}),h.appendChild(w),h.appendChild(C),$(u,C),v.appendChild(h),l.appendChild(v),e.toolBtnScreenshot.classList.add(s+"tool-active"),e.shadow.appendChild(l),e.activePopover=l;let ce={x:window.scrollX+window.innerWidth/2-160,y:window.scrollY+window.innerHeight/3,width:320,height:0};Ye(l,ce),u.focus()}var Qt,ke=B(()=>{"use strict";g();k();x();Z();X();ye()});var oo={};lt(oo,{setMode:()=>W});function W(t){console.log("[veld] setMode:",t),e.activeMode==="select-element"&&(e.overlay.classList.remove(s+"overlay-active"),e.hoverOutline.style.display="none",e.componentTraceEl.style.display="none",e.hoveredEl=null,e.lockedEl=null),e.activeMode==="screenshot"&&(e.overlay.classList.remove(s+"overlay-active"),e.overlay.classList.remove(s+"overlay-crosshair"),e.screenshotRect.style.display="none",K()),e.activeMode==="draw"&&(Ve(),K()),M(),e.activeMode=t,e.toolBtnSelect.classList.toggle(s+"tool-active",t==="select-element"),e.toolBtnScreenshot.classList.toggle(s+"tool-active",t==="screenshot"),e.toolBtnDraw.classList.toggle(s+"tool-active",t==="draw"),t==="select-element"&&e.overlay.classList.add(s+"overlay-active"),t==="screenshot"&&$e().then(()=>{e.overlay.classList.add(s+"overlay-active"),e.overlay.classList.add(s+"overlay-crosshair"),window.focus(),m("Draw a rectangle to capture a screenshot")}).catch(()=>{m("Screen capture denied",!0),e.activeMode=null,e.toolBtnScreenshot.classList.remove(s+"tool-active")}),t==="draw"&&(e.toolbarOpen&&J(),$e().then(()=>Ge()).then(()=>{We(),window.focus()}).catch(()=>{m("Screen capture denied",!0),e.activeMode=null,e.toolBtnDraw.classList.remove(s+"tool-active")}))}var Je=B(()=>{"use strict";g();x();X();ye();ke();we();be()});var Pe=`/* Veld Feedback Overlay Styles */

[class^="veld-feedback-"],
[class*=" veld-feedback-"] {
  box-sizing: border-box;
}

/* Reverse dark mode: veld UI contrasts with the page.
   Dark OS = light page = dark veld UI.
   Light OS = dark page = light veld UI. */

/* Default (dark UI \u2014 for light pages) */
:host {
  --vf-bg: #0a0a0a;
  --vf-bg-card: #1e1e24;
  --vf-accent: #C4F56A;
  --vf-accent-hover: #a8d94f;
  --vf-accent-text: #0a0a0a;
  --vf-danger: #ef4444;
  --vf-danger-text: #fff;
  --vf-text: #f1f5f9;
  --vf-text-muted: #94a3b8;
  --vf-border: #2a2a30;
  --vf-shadow: 0 4px 20px rgba(0,0,0,.4);
  --vf-radius: 10px;
  --vf-z: 999999;
  --vf-font: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif;
}

/* Dark OS \u2192 dark pages \u2192 light veld UI */
@media (prefers-color-scheme: dark) {
  :host {
    --vf-bg: #f8f8fa;
    --vf-bg-card: #eeeef2;
    --vf-accent: #16a34a;
    --vf-accent-hover: #15803d;
    --vf-accent-text: #fff;
    --vf-danger: #dc2626;
    --vf-danger-text: #fff;
    --vf-text: #1a1a2e;
    --vf-text-muted: #64748b;
    --vf-border: #d4d4d8;
    --vf-shadow: 0 4px 20px rgba(0,0,0,.1);
  }
}

/* Manual theme overrides (data-theme attribute on host element) */
:host([data-theme="dark"]) {
  --vf-bg: #0a0a0a;
  --vf-bg-card: #1e1e24;
  --vf-accent: #C4F56A;
  --vf-accent-hover: #a8d94f;
  --vf-accent-text: #0a0a0a;
  --vf-danger: #ef4444;
  --vf-danger-text: #fff;
  --vf-text: #f1f5f9;
  --vf-text-muted: #94a3b8;
  --vf-border: #2a2a30;
  --vf-shadow: 0 4px 20px rgba(0,0,0,.4);
}
:host([data-theme="light"]) {
  --vf-bg: #f8f8fa;
  --vf-bg-card: #eeeef2;
  --vf-accent: #16a34a;
  --vf-accent-hover: #15803d;
  --vf-accent-text: #fff;
  --vf-danger: #dc2626;
  --vf-danger-text: #fff;
  --vf-text: #1a1a2e;
  --vf-text-muted: #64748b;
  --vf-border: #d4d4d8;
  --vf-shadow: 0 4px 20px rgba(0,0,0,.1);
}

/* Hide/show transition */
.veld-feedback-toolbar-container,
.veld-feedback-pin {
  transition: opacity .2s ease, transform .2s ease;
}
.veld-feedback-hidden {
  opacity: 0 !important;
  transform: scale(0.85) !important;
  pointer-events: none !important;
}

/* Toast */
.veld-feedback-toast {
  position: fixed; bottom: 100px; left: 50%;
  transform: translateX(-50%) translateY(20px);
  background: var(--vf-accent); color: var(--vf-accent-text);
  padding: 10px 22px; border-radius: 8px;
  font: 500 13px/1.4 var(--vf-font);
  z-index: calc(var(--vf-z) + 10);
  opacity: 0; transition: opacity .3s, transform .3s;
  pointer-events: none; white-space: nowrap;
}
.veld-feedback-toast-show {
  opacity: 1; transform: translateX(-50%) translateY(0);
}

/* Toolbar container \u2014 fixed, draggable */
.veld-feedback-toolbar-container {
  position: fixed;
  display: flex; align-items: center;
  z-index: var(--vf-z);
  user-select: none;
}

.veld-feedback-toolbar-container.veld-feedback-toolbar-right {
  flex-direction: row;
}
.veld-feedback-toolbar-container.veld-feedback-toolbar-left {
  flex-direction: row-reverse;
}

/* FAB */
.veld-feedback-fab {
  width: 40px; height: 40px; border-radius: 50%;
  border: 1px solid var(--vf-border);
  cursor: pointer; background: var(--vf-bg);
  color: var(--vf-text); display: flex;
  align-items: center; justify-content: center;
  box-shadow: var(--vf-shadow); transition: background .2s, transform .15s;
  position: relative; z-index: 2;
  flex-shrink: 0;
}
.veld-feedback-fab:hover {
  background: var(--vf-bg-card); transform: scale(1.07);
}
.veld-feedback-fab svg {
  width: 20px; height: 20px;
}
.veld-feedback-fab-pulse {
  animation: veld-feedback-fab-glow 2s ease-in-out infinite;
}
@keyframes veld-feedback-fab-glow {
  0%, 100% { box-shadow: 0 0 0 0 rgba(196, 245, 106, 0.5); }
  50% { box-shadow: 0 0 0 10px rgba(196, 245, 106, 0); }
}

/* Badge */
.veld-feedback-badge {
  position: absolute; top: -4px; right: -4px;
  min-width: 16px; height: 16px; border-radius: 8px;
  background: var(--vf-danger); color: var(--vf-danger-text);
  font: 700 10px/1 var(--vf-font);
  display: flex; align-items: center; justify-content: center;
  padding: 0 4px; pointer-events: none;
}
.veld-feedback-badge-hidden { display: none; }

/* Toolbar pill */
.veld-feedback-toolbar {
  display: flex; align-items: center; gap: 2px;
  background: var(--vf-bg); border: 1px solid var(--vf-border);
  border-radius: 24px; padding: 4px;
  box-shadow: var(--vf-shadow);
  overflow: hidden;
  max-width: 0; opacity: 0;
  transition: max-width .2s ease, opacity .2s ease, padding .2s ease;
}
.veld-feedback-toolbar-open {
  max-width: 550px; opacity: 1; padding: 4px 6px;
}

/* Gap between toolbar and FAB */
.veld-feedback-toolbar-right .veld-feedback-toolbar {
  margin-right: 8px;
}
.veld-feedback-toolbar-left .veld-feedback-toolbar {
  margin-left: 8px;
}

/* Toolbar button */
.veld-feedback-tool-btn {
  width: 30px; height: 30px; border-radius: 50%; border: none;
  cursor: pointer; background: transparent;
  color: var(--vf-text-muted); display: flex;
  align-items: center; justify-content: center;
  transition: background .15s, color .15s;
  flex-shrink: 0;
}
.veld-feedback-tool-btn:hover {
  background: var(--vf-bg-card); color: var(--vf-text);
}
.veld-feedback-tool-btn.veld-feedback-tool-active {
  background: var(--vf-accent); color: var(--vf-accent-text);
}
.veld-feedback-tool-btn svg {
  width: 15px; height: 15px;
}

/* Separator in toolbar */
.veld-feedback-separator {
  width: 1px; height: 20px;
  background: var(--vf-border); margin: 0 4px;
  flex-shrink: 0;
}

/* Feedback mode backdrop */
.veld-feedback-overlay {
  position: fixed; inset: 0;
  z-index: calc(var(--vf-z) - 2);
  background: rgba(10,10,10,.08);
  display: none;
  cursor: pointer;
}
.veld-feedback-overlay-active { display: block; }

/* Hover outline */
.veld-feedback-hover-outline {
  position: absolute;
  border: 2px dashed var(--vf-accent);
  outline: 2px dashed rgba(0, 0, 0, 0.6);
  outline-offset: 2px;
  pointer-events: none;
  z-index: calc(var(--vf-z) - 1);
  border-radius: 3px;
  transition: top .1s, left .1s, width .1s, height .1s;
  display: none;
}

/* Component trace tooltip */
.veld-feedback-component-trace {
  position: absolute;
  z-index: var(--vf-z);
  background: var(--vf-bg);
  color: var(--vf-accent);
  padding: 4px 10px;
  border-radius: 6px;
  font: 500 11px/1.4 var(--vf-font);
  pointer-events: none;
  white-space: nowrap;
  box-shadow: 0 2px 10px rgba(0,0,0,.4);
  border: 1px solid var(--vf-border);
  display: none;
}

/* Popover */
.veld-feedback-popover {
  position: absolute; z-index: var(--vf-z);
  width: 360px; max-width: calc(100vw - 32px);
  background: var(--vf-bg); color: var(--vf-text);
  border-radius: var(--vf-radius);
  border: 1px solid var(--vf-border);
  box-shadow: var(--vf-shadow);
  font: 13px/1.5 var(--vf-font);
  overflow: hidden;
  animation: veld-feedback-fadeIn .15s ease;
}
@keyframes veld-feedback-fadeIn {
  from { opacity: 0; transform: translateY(6px); }
  to { opacity: 1; transform: translateY(0); }
}

.veld-feedback-popover-header {
  padding: 10px 14px; background: var(--vf-bg-card);
  font-size: 11px; color: var(--vf-text-muted);
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  border-bottom: 1px solid var(--vf-border);
}

.veld-feedback-popover-selector {
  padding: 10px 14px 4px; background: var(--vf-bg-card);
  font-size: 11px; color: var(--vf-text-muted);
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}

.veld-feedback-popover-trace {
  padding: 2px 14px 10px; background: var(--vf-bg-card);
  font-size: 10px; color: var(--vf-accent);
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  border-bottom: 1px solid var(--vf-border);
}

.veld-feedback-popover-body { padding: 14px; }

.veld-feedback-textarea {
  width: 100%; min-height: 80px; resize: vertical;
  background: var(--vf-bg-card); color: var(--vf-text);
  border: 1px solid var(--vf-border); border-radius: 6px;
  padding: 8px 10px; font: inherit; outline: none;
  box-sizing: border-box;
}
.veld-feedback-textarea:focus {
  border-color: var(--vf-accent);
}

.veld-feedback-popover-actions {
  display: flex; gap: 8px; justify-content: flex-end; margin-top: 10px;
}

/* Buttons */
.veld-feedback-btn {
  padding: 6px 14px; border-radius: 6px; border: none; cursor: pointer;
  font: 500 12px/1.4 var(--vf-font); transition: background .15s;
}
.veld-feedback-btn-primary { background: var(--vf-accent); color: var(--vf-accent-text); }
.veld-feedback-btn-primary:hover { background: var(--vf-accent-hover); }
.veld-feedback-btn-secondary { background: var(--vf-border); color: var(--vf-text); }
.veld-feedback-btn-secondary:hover { background: var(--vf-border); }
.veld-feedback-btn-danger { background: var(--vf-danger); color: var(--vf-danger-text); }
.veld-feedback-btn-danger:hover { opacity: 0.85; }
.veld-feedback-btn-sm { padding: 4px 10px; font-size: 11px; }

/* --- Pins (thread-aware) --- */
.veld-feedback-pin {
  position: absolute;
  z-index: calc(var(--vf-z) - 1);
  display: flex; align-items: center; gap: 3px;
  padding: 3px 8px;
  background: var(--vf-bg); color: var(--vf-text);
  border: 1px solid var(--vf-border);
  border-radius: 16px;
  cursor: pointer; box-shadow: 0 2px 8px rgba(0,0,0,.4);
  transition: transform .15s, border-color .15s;
  font: 500 10px/1 var(--vf-font);
}
.veld-feedback-pin:hover {
  transform: scale(1.1); border-color: var(--vf-accent);
}
.veld-feedback-pin-icon svg {
  width: 14px; height: 14px; color: var(--vf-accent);
}
.veld-feedback-pin-count {
  font: 700 10px/1 var(--vf-font); color: var(--vf-text-muted);
}
.veld-feedback-pin-unread-dot {
  width: 7px; height: 7px; border-radius: 50%;
  background: var(--vf-danger); flex-shrink: 0;
}
.veld-feedback-pin-highlight {
  animation: veld-feedback-pin-pulse 1.5s ease;
}
@keyframes veld-feedback-pin-pulse {
  0% { box-shadow: 0 0 0 0 rgba(196, 245, 106, 0.7); transform: scale(1); }
  20% { box-shadow: 0 0 0 8px rgba(196, 245, 106, 0.4); transform: scale(1.15); }
  50% { box-shadow: 0 0 0 12px rgba(196, 245, 106, 0); transform: scale(1.1); }
  100% { box-shadow: 0 0 0 0 rgba(196, 245, 106, 0); transform: scale(1); }
}

/* Side panel */
.veld-feedback-panel {
  position: fixed; top: 0; right: 0; bottom: 0;
  width: 380px; max-width: 90vw;
  background: var(--vf-bg); color: var(--vf-text);
  z-index: var(--vf-z);
  box-shadow: -4px 0 30px rgba(0,0,0,.5);
  display: flex; flex-direction: column;
  font: 13px/1.5 var(--vf-font);
  transform: translateX(100%); transition: transform .25s ease;
  border-left: 1px solid var(--vf-border);
}
.veld-feedback-panel-open { transform: translateX(0); }

.veld-feedback-panel-head {
  padding: 14px 18px; font-size: 14px; font-weight: 600;
  border-bottom: 1px solid var(--vf-border);
  display: flex; align-items: center; gap: 12px;
  min-height: 52px;
}
.veld-feedback-panel-back-btn {
  width: 28px; height: 28px; border-radius: 6px; border: none;
  background: transparent; color: var(--vf-text-muted);
  cursor: pointer; display: flex; align-items: center; justify-content: center;
  flex-shrink: 0; transition: background .15s, color .15s;
}
.veld-feedback-panel-back-btn:hover {
  background: var(--vf-bg-card); color: var(--vf-text);
}
.veld-feedback-panel-back-btn svg { width: 16px; height: 16px; }
.veld-feedback-panel-head-title { flex-shrink: 0; }
.veld-feedback-panel-mark-read {
  width: 28px; height: 28px; border-radius: 6px; border: none;
  background: transparent; color: var(--vf-text-muted);
  cursor: pointer; display: flex; align-items: center; justify-content: center;
  flex-shrink: 0; transition: background .15s, color .15s;
}
.veld-feedback-panel-mark-read:hover {
  background: var(--vf-bg-card); color: var(--vf-accent);
}
.veld-feedback-panel-mark-read svg { width: 16px; height: 16px; }
.veld-feedback-panel-close {
  background: none; border: none; color: var(--vf-text-muted);
  cursor: pointer; font-size: 20px; line-height: 1; padding: 0 2px;
  margin-left: auto;
}
.veld-feedback-panel-close:hover { color: var(--vf-text); }

.veld-feedback-panel-body {
  flex: 1; overflow-y: auto; padding: 12px 18px;
}

.veld-feedback-panel-empty {
  text-align: center; color: var(--vf-text-muted);
  padding: 40px 0; font-size: 12px;
}

/* --- Panel detail view (two-layer) --- */
.veld-feedback-panel-detail-header {
  margin-bottom: 8px; padding-bottom: 10px;
  border-bottom: 1px solid var(--vf-border);
  display: flex; flex-direction: column; gap: 3px;
}
.veld-feedback-panel-detail-id {
  font-size: 9px; color: var(--vf-text-muted);
  cursor: pointer; font-family: var(--vf-font);
  display: flex; align-items: center; gap: 4px;
  opacity: 0.5; transition: opacity .15s;
}
.veld-feedback-panel-detail-id:hover { opacity: 1; color: var(--vf-accent); }
.veld-feedback-panel-detail-title {
  font-size: 13px; font-weight: 600; color: var(--vf-text);
  margin-top: 2px;
}
.veld-feedback-panel-detail-page-link {
  display: inline-block;
  font-size: 11px; color: var(--vf-accent); cursor: pointer;
  text-decoration: none;
}
.veld-feedback-panel-detail-page-link:hover { text-decoration: underline; }
.veld-feedback-panel-detail-trace,
.veld-feedback-panel-detail-selector {
  font-size: 10px; color: var(--vf-text-muted);
  cursor: pointer; font-family: var(--vf-font);
  display: -webkit-box; -webkit-box-orient: vertical;
  -webkit-line-clamp: 2; overflow: hidden;
  word-break: break-all;
  opacity: 0.7; transition: opacity .15s;
}
.veld-feedback-panel-detail-trace:hover,
.veld-feedback-panel-detail-selector:hover { opacity: 1; color: var(--vf-accent); }
.veld-feedback-panel-detail-trace { color: var(--vf-accent); opacity: 0.8; }
.veld-feedback-panel-detail-copy-icon {
  display: inline; vertical-align: middle; margin-left: 4px;
}
.veld-feedback-panel-detail-copy-icon svg {
  width: 11px; height: 11px;
}

/* --- Segmented control --- */
.veld-feedback-segmented {
  display: flex; background: var(--vf-bg-card); border-radius: 6px; padding: 2px;
  gap: 2px; flex: 1;
}
.veld-feedback-segmented-btn {
  flex: 1; padding: 4px 12px; border: none; border-radius: 4px;
  background: transparent; color: var(--vf-text-muted);
  font: 500 11px/1.4 var(--vf-font); cursor: pointer;
  transition: background .15s, color .15s;
}
.veld-feedback-segmented-btn:not(.veld-feedback-segmented-btn-active):hover {
  background: rgba(255,255,255,.05);
}
.veld-feedback-segmented-btn-active {
  background: var(--vf-accent); color: var(--vf-accent-text);
}

/* --- Panel section (page group) --- */
.veld-feedback-panel-section { margin-bottom: 12px; }
.veld-feedback-panel-section-heading {
  font-size: 11px; color: var(--vf-text-muted);
  padding: 4px 0 8px; margin-bottom: 4px;
  border-bottom: 1px solid var(--vf-border);
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}

/* --- Thread card (compact) --- */
.veld-feedback-thread-card {
  background: var(--vf-bg-card); border-radius: 8px;
  padding: 8px 12px; margin-bottom: 6px;
  border: 1px solid var(--vf-border); cursor: pointer;
  transition: border-color .15s;
}
.veld-feedback-thread-card:hover { border-color: var(--vf-accent); }
.veld-feedback-thread-card-resolved { opacity: 0.45; cursor: pointer; }
.veld-feedback-thread-card-resolved:hover { opacity: 0.7; }
.veld-feedback-thread-card-unread { border-left: 3px solid var(--vf-danger); }
.veld-feedback-thread-card-row {
  display: flex; align-items: baseline; gap: 8px;
}
.veld-feedback-thread-card-preview {
  font-size: 12px; color: var(--vf-text);
  flex: 1; min-width: 0;
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}
.veld-feedback-thread-card-meta {
  font-size: 10px; color: var(--vf-text-muted); white-space: nowrap; flex-shrink: 0;
}
.veld-feedback-thread-card-selector {
  font-size: 10px; color: var(--vf-text-muted); margin-top: 3px;
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}

/* --- Thread messages --- */
.veld-feedback-thread-messages {
  padding: 8px 0;
}
.veld-feedback-thread-messages-list {
  max-height: 300px; overflow-y: auto;
}
.veld-feedback-message {
  display: flex; gap: 8px; margin-bottom: 10px; align-items: flex-start;
}
.veld-feedback-message-author-icon {
  width: 24px; height: 24px; border-radius: 50%;
  display: flex; align-items: center; justify-content: center;
  flex-shrink: 0;
}
.veld-feedback-message-author-icon svg {
  width: 12px; height: 12px;
}
.veld-feedback-message-human .veld-feedback-message-author-icon {
  background: var(--vf-accent); color: var(--vf-accent-text);
}
.veld-feedback-message-agent .veld-feedback-message-author-icon {
  background: var(--vf-bg-card); color: var(--vf-text-muted);
  border: 1px solid var(--vf-border);
}
.veld-feedback-message-body { flex: 1; min-width: 0; }
.veld-feedback-message-text {
  font-size: 12px; line-height: 1.5; color: var(--vf-text);
  white-space: pre-wrap; word-break: break-word;
}
.veld-feedback-message-meta {
  font-size: 10px; color: var(--vf-text-muted); margin-top: 2px;
}

/* --- Thread input (reply) --- */
.veld-feedback-thread-input {
  border-top: 1px solid var(--vf-border); padding-top: 8px; margin-top: 8px;
}
.veld-feedback-thread-input .veld-feedback-textarea {
  min-height: 50px; font-size: 12px;
}
.veld-feedback-thread-input-actions {
  display: flex; gap: 6px; justify-content: flex-end; margin-top: 6px;
}

/* --- Listening module (in toolbar) --- */
.veld-feedback-listening {
  display: none; /* toggled to flex via JS */
  align-items: center; gap: 8px;
}
.veld-feedback-listening-dot {
  width: 10px; height: 10px; border-radius: 50%;
  background: var(--vf-accent); flex-shrink: 0;
  animation: veld-feedback-pulse-dot 2s ease-in-out infinite;
  cursor: default;
}
@keyframes veld-feedback-pulse-dot {
  0%, 100% { box-shadow: 0 0 0 0 rgba(196, 245, 106, 0.6); }
  50% { box-shadow: 0 0 0 5px rgba(196, 245, 106, 0); }
}
.veld-feedback-listening-allgood {
  padding: 3px 8px; border-radius: 6px; border: none;
  background: var(--vf-accent); color: var(--vf-accent-text);
  font: 500 10px/1.4 var(--vf-font); cursor: pointer;
  transition: background .15s; flex-shrink: 0;
}
.veld-feedback-listening-allgood:hover { background: var(--vf-accent-hover); }

/* --- Agent reply toast --- */
.veld-feedback-agent-toast {
  position: fixed; bottom: 100px; left: 50%;
  transform: translateX(-50%) translateY(20px);
  background: var(--vf-bg); color: var(--vf-text);
  border: 1px solid var(--vf-border);
  padding: 12px 16px; border-radius: 10px;
  font: 13px/1.4 var(--vf-font);
  z-index: calc(var(--vf-z) + 10);
  opacity: 0; transition: opacity .3s, transform .3s;
  pointer-events: auto; max-width: 360px;
  box-shadow: var(--vf-shadow);
}
.veld-feedback-agent-toast-show {
  opacity: 1; transform: translateX(-50%) translateY(0);
}
.veld-feedback-agent-toast-header {
  font-size: 11px; color: var(--vf-accent); font-weight: 600; margin-bottom: 4px;
}
.veld-feedback-agent-toast-body {
  font-size: 12px; color: var(--vf-text-muted);
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  margin-bottom: 6px;
}
.veld-feedback-agent-toast-link {
  background: none; border: none; color: var(--vf-accent);
  font: 500 11px/1 var(--vf-font); cursor: pointer; padding: 0;
  text-decoration: underline;
}

/* Screenshot selection rectangle */
.veld-feedback-screenshot-rect {
  position: absolute;
  border: 2px dashed var(--vf-accent);
  outline: 2px dashed rgba(0, 0, 0, 0.6);
  outline-offset: 2px;
  background: rgba(196, 245, 106, 0.08);
  pointer-events: none;
  z-index: calc(var(--vf-z) - 1);
  border-radius: 3px;
  display: none;
}

/* Crosshair cursor on backdrop during screenshot mode */
.veld-feedback-overlay-crosshair {
  cursor: crosshair;
}

/* Screenshot popover \u2014 clamped to viewport, centered */
.veld-feedback-popover-screenshot {
  position: fixed !important;
  top: 20px !important; left: 50% !important;
  transform: translateX(-50%) !important;
  width: auto;
  max-width: calc(100vw - 60px);
  max-height: calc(100vh - 40px);
  display: flex; flex-direction: column;
  overflow-y: auto;
}

/* Screenshot preview in popover */
.veld-feedback-screenshot-preview {
  position: relative;
  padding: 10px;
  background: var(--vf-bg-card);
  border-bottom: 1px solid var(--vf-border);
  overflow: hidden;
  flex-shrink: 1;
  min-height: 0;
}
.veld-feedback-screenshot-img {
  display: block;
  border-radius: 4px;
  max-width: 100%;
  max-height: calc(100vh - 300px);
  object-fit: contain;
}

/* Tooltip */
.veld-feedback-tooltip {
  position: absolute;
  z-index: calc(var(--vf-z) + 20);
  background: var(--vf-bg);
  color: var(--vf-text);
  padding: 5px 10px;
  border-radius: 6px;
  font: 500 11px/1.4 var(--vf-font);
  white-space: nowrap;
  box-shadow: 0 2px 12px rgba(0,0,0,.5);
  border: 1px solid var(--vf-border);
  pointer-events: none;
  display: none;
}

/* Kbd shortcut keys */
.veld-feedback-kbd-group {
  display: inline-flex;
  gap: 3px;
  margin-left: 8px;
  vertical-align: baseline;
}
.veld-feedback-kbd {
  display: inline-flex;
  align-items: center; justify-content: center;
  min-width: 20px; height: 20px;
  background: var(--vf-bg-card);
  color: var(--vf-text-muted);
  font: 500 10px/1 var(--vf-font);
  padding: 0 5px;
  border-radius: 4px;
  border: 1px solid var(--vf-border);
  box-shadow: 0 1px 0 rgba(255,255,255,.06);
}

/* Confirm modal */
.veld-feedback-confirm-backdrop {
  position: fixed; inset: 0;
  z-index: calc(var(--vf-z) + 40);
  background: rgba(0, 0, 0, 0.6);
  display: flex; align-items: center; justify-content: center;
  opacity: 0; transition: opacity .2s ease;
}
.veld-feedback-confirm-backdrop-visible {
  opacity: 1;
}
.veld-feedback-confirm-modal {
  background: var(--vf-bg);
  border: 1px solid var(--vf-border);
  border-radius: var(--vf-radius);
  padding: 24px 28px;
  max-width: 380px; width: 90vw;
  box-shadow: var(--vf-shadow);
  font: 13px/1.5 var(--vf-font);
  color: var(--vf-text);
  transform: scale(0.92) translateY(12px);
  transition: transform .2s ease;
}
.veld-feedback-confirm-backdrop-visible .veld-feedback-confirm-modal {
  transform: scale(1) translateY(0);
}
.veld-feedback-confirm-message {
  margin-bottom: 20px;
  font-size: 13px; line-height: 1.5;
}
.veld-feedback-confirm-actions {
  display: flex; gap: 8px; justify-content: flex-end;
}

/* ---- Draw mode ---- */

/* Full-viewport draw canvas (global mode) */
.veld-feedback-draw-canvas {
  position: fixed; inset: 0;
  width: 100%; height: 100%;
  z-index: calc(var(--vf-z) - 2);
  cursor: crosshair;
  touch-action: none;
  background: transparent;
}

/* Inline draw canvas (screenshot annotation) */
.veld-feedback-draw-canvas-inline {
  position: absolute; top: 0; left: 0;
  width: 100%; height: 100%;
  cursor: crosshair;
  touch-action: none;
}

/* Draw toolbar */
.veld-feedback-draw-toolbar {
  position: fixed; top: 16px; left: 50%;
  transform: translateX(-50%);
  display: flex; align-items: center; gap: 4px;
  background: var(--vf-bg);
  border: 1px solid var(--vf-border);
  border-radius: 24px; padding: 5px 8px;
  box-shadow: var(--vf-shadow);
  z-index: calc(var(--vf-z) + 1);
  font: 12px var(--vf-font);
  color: var(--vf-text);
  user-select: none;
  transition: padding .2s ease, opacity .2s ease;
}
/* Semi-transparent when idle on desktop, full on hover */
@media (hover: hover) {
  .veld-feedback-draw-toolbar { opacity: 0.25; }
  .veld-feedback-draw-toolbar:hover { opacity: 1; }
}

/* Collapse button */
.veld-feedback-draw-collapse-btn {
  width: 24px; height: 24px; border-radius: 6px;
  border: none; cursor: pointer;
  background: transparent;
  color: var(--vf-text-muted);
  display: flex; align-items: center; justify-content: center;
  padding: 0; outline: none; flex-shrink: 0;
  transition: transform .2s ease, color .15s;
}
.veld-feedback-draw-collapse-btn svg { width: 14px; height: 14px; }
.veld-feedback-draw-collapse-btn:hover { color: var(--vf-text); }
.veld-feedback-draw-collapse-collapsed { transform: rotate(180deg); }

/* Collapsible tools container \u2014 scrollable on narrow viewports */
.veld-feedback-draw-tools-wrap {
  display: flex; align-items: center; gap: 4px;
  overflow-x: auto; overflow-y: hidden;
  max-width: calc(100vw - 160px);
  scrollbar-width: none;
}
.veld-feedback-draw-tools-wrap::-webkit-scrollbar { display: none; }

/* Color swatch buttons */
.veld-feedback-draw-color {
  width: 22px; height: 22px; border-radius: 50%;
  border: 2px solid var(--vf-border);
  margin: 0 1px;
  cursor: pointer; flex-shrink: 0;
  transition: border-color .15s, transform .1s, box-shadow .15s;
  padding: 0; outline: none;
  box-shadow: 0 0 0 1px rgba(0,0,0,.08);
}
.veld-feedback-draw-color:hover {
  transform: scale(1.15);
  border-color: var(--vf-text-muted);
}
.veld-feedback-draw-color-active {
  border-color: var(--vf-accent) !important;
  box-shadow: inset 0 0 0 2px var(--vf-bg), 0 0 0 0 transparent;
}

/* Separator */
.veld-feedback-draw-sep {
  width: 1px; height: 18px;
  background: var(--vf-border);
  margin: 0 6px; flex-shrink: 0;
}

/* Thickness buttons */
.veld-feedback-draw-thick {
  width: 26px; height: 26px; border-radius: 50%;
  border: none; cursor: pointer;
  background: transparent;
  display: flex; align-items: center; justify-content: center;
  padding: 0; outline: none;
  transition: background .15s;
}
.veld-feedback-draw-thick:hover {
  background: rgba(255,255,255,0.06);
}
.veld-feedback-draw-thick-active {
  background: var(--vf-bg-card);
}
.veld-feedback-draw-thick-dot {
  border-radius: 50%;
  background: var(--vf-text);
  display: block;
}

/* Tool buttons (eraser, undo, redo) */
.veld-feedback-draw-tool-btn {
  width: 30px; height: 30px; border-radius: 8px;
  border: 1px solid transparent; cursor: pointer;
  background: transparent;
  color: var(--vf-text-muted);
  display: flex; align-items: center; justify-content: center;
  padding: 0; outline: none;
  transition: background .15s, color .15s;
}
.veld-feedback-draw-tool-btn svg {
  width: 16px; height: 16px;
}
.veld-feedback-draw-tool-btn:hover {
  background: var(--vf-bg-card);
  border-color: var(--vf-border);
  color: var(--vf-text);
}
.veld-feedback-draw-tool-btn-active {
  background: var(--vf-bg-card);
  color: var(--vf-accent);
}
.veld-feedback-draw-tool-btn:disabled {
  opacity: 0.3; cursor: default;
}
.veld-feedback-draw-tool-btn:disabled:hover {
  background: transparent;
  color: var(--vf-text-muted);
}

/* Done button */
.veld-feedback-draw-done-btn {
  height: 28px; border-radius: 14px;
  border: none; cursor: pointer;
  background: var(--vf-accent);
  color: var(--vf-bg);
  font: 12px/1 var(--vf-font);
  font-weight: 600;
  display: flex; align-items: center; gap: 4px;
  padding: 0 12px 0 8px;
  outline: none;
  transition: background .15s;
}
.veld-feedback-draw-done-btn svg {
  width: 14px; height: 14px;
}
.veld-feedback-draw-done-btn:hover {
  background: var(--vf-accent-hover);
}

/* Draw toolbar when inside a container (annotation mode) */
.veld-feedback-screenshot-preview .veld-feedback-draw-toolbar {
  position: absolute; top: auto; bottom: -44px; left: 50%;
  transform: translateX(-50%);
}

/* Annotate button in screenshot preview */
.veld-feedback-annotate-btn {
  position: absolute; top: 16px; right: 16px;
  height: 26px; border-radius: 13px;
  border: 1px solid rgba(255,255,255,0.15);
  cursor: pointer;
  background: rgba(10,10,10,0.75);
  backdrop-filter: blur(8px);
  -webkit-backdrop-filter: blur(8px);
  color: var(--vf-text);
  font: 11px/1 var(--vf-font);
  padding: 0 10px;
  display: flex; align-items: center; gap: 4px;
  outline: none;
  transition: background .15s, border-color .15s;
}
.veld-feedback-annotate-btn:hover {
  background: rgba(10,10,10,0.9);
  border-color: var(--vf-accent);
}
.veld-feedback-annotate-btn svg {
  width: 12px; height: 12px;
}
`;var st=`
/* Theme variables for light DOM \u2014 inherited from veld-feedback host */
veld-feedback {
  --vfl-bg: #0a0a0a;
  --vfl-bg-card: #1e1e24;
  --vfl-text: #f1f5f9;
  --vfl-text-muted: #94a3b8;
  --vfl-accent: #C4F56A;
  --vfl-danger: #ef4444;
  --vfl-border: #2a2a30;
  --vfl-font: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif;
}
@media (prefers-color-scheme: dark) {
  veld-feedback {
    --vfl-bg: #f8f8fa;
    --vfl-bg-card: #eeeef2;
    --vfl-text: #1a1a2e;
    --vfl-text-muted: #64748b;
    --vfl-accent: #16a34a;
    --vfl-danger: #dc2626;
    --vfl-border: #d4d4d8;
  }
}
veld-feedback[data-theme="dark"] {
  --vfl-bg: #0a0a0a;
  --vfl-bg-card: #1e1e24;
  --vfl-text: #f1f5f9;
  --vfl-text-muted: #94a3b8;
  --vfl-accent: #C4F56A;
  --vfl-danger: #ef4444;
  --vfl-border: #2a2a30;
}
veld-feedback[data-theme="light"] {
  --vfl-bg: #f8f8fa;
  --vfl-bg-card: #eeeef2;
  --vfl-text: #1a1a2e;
  --vfl-text-muted: #64748b;
  --vfl-accent: #16a34a;
  --vfl-danger: #dc2626;
  --vfl-border: #d4d4d8;
}

[class^="veld-feedback-"],
[class*=" veld-feedback-"] {
  box-sizing: border-box;
}
.veld-feedback-overlay {
  position: fixed; inset: 0;
  z-index: 999997;
  background: rgba(10,10,10,.08);
  display: none; cursor: pointer;
}
.veld-feedback-overlay-active { display: block; }
.veld-feedback-overlay-crosshair { cursor: crosshair; }
.veld-feedback-hover-outline {
  position: absolute;
  border: 2px dashed var(--vfl-accent);
  outline: 2px dashed rgba(0,0,0,0.4);
  outline-offset: 2px;
  pointer-events: none;
  z-index: 999998;
  border-radius: 3px;
  transition: top .1s, left .1s, width .1s, height .1s;
  display: none;
}
.veld-feedback-component-trace {
  position: absolute; z-index: 999999;
  background: var(--vfl-bg); color: var(--vfl-accent);
  padding: 4px 10px; border-radius: 6px;
  font: 500 11px/1.4 var(--vfl-font);
  pointer-events: none; white-space: nowrap;
  box-shadow: 0 2px 10px rgba(0,0,0,.15);
  border: 1px solid var(--vfl-border);
  display: none;
}
.veld-feedback-screenshot-rect {
  position: absolute;
  border: 2px dashed var(--vfl-accent);
  outline: 2px dashed rgba(0,0,0,0.4);
  outline-offset: 2px;
  background: rgba(100,100,100,.06);
  pointer-events: none;
  z-index: 999998;
  border-radius: 3px;
  display: none;
}
.veld-feedback-draw-canvas {
  position: fixed; inset: 0;
  width: 100%; height: 100%;
  z-index: 999997;
  cursor: crosshair;
  touch-action: none;
  background: transparent;
}
.veld-feedback-pin {
  position: absolute; z-index: 999998;
  display: flex; align-items: center; gap: 3px;
  padding: 3px 8px;
  background: var(--vfl-bg); color: var(--vfl-text);
  border: 1px solid var(--vfl-border);
  border-radius: 16px;
  cursor: pointer;
  box-shadow: 0 2px 8px rgba(0,0,0,.12);
  transition: transform .15s, border-color .15s;
  font: 500 10px/1 var(--vfl-font);
}
.veld-feedback-pin:hover { transform: scale(1.1); border-color: var(--vfl-accent); }
.veld-feedback-pin-icon svg { width: 14px; height: 14px; color: var(--vfl-accent); }
.veld-feedback-pin-count { font: 700 10px/1 var(--vfl-font); color: var(--vfl-text-muted); }
.veld-feedback-pin-unread-dot { width: 7px; height: 7px; border-radius: 50%; background: var(--vfl-danger); flex-shrink: 0; }
.veld-feedback-pin-highlight { animation: veld-feedback-pin-pulse 1.5s ease; }
@keyframes veld-feedback-pin-pulse {
  0% { box-shadow: 0 0 0 0 rgba(100,200,100,.5); transform: scale(1); }
  50% { box-shadow: 0 0 0 10px rgba(100,200,100,0); transform: scale(1.1); }
  100% { box-shadow: 0 0 0 0 rgba(100,200,100,0); transform: scale(1); }
}
.veld-feedback-hidden {
  opacity: 0 !important;
  transform: scale(0.85) !important;
  pointer-events: none !important;
}
`;g();g();x();g();k();x();me();X();g();x();k();function Se(t){let o=[],a=fo(t);if(a){let r=a,d=0;for(;r&&d++<100;){let l=vo(r);l&&o.unshift(l),r=r.return}if(o.length)return o}if(t.__vueParentComponent){let r=t.__vueParentComponent,d=0;for(;r&&d++<100;){let l=r.type&&(r.type.name||r.type.__name);l&&o.unshift(l),r=r.parent}if(o.length)return o}if(t.__vue__){let r=t.__vue__,d=0;for(;r&&d++<100;){let l=r.$options&&r.$options.name;l&&o.unshift(l),r=r.$parent}if(o.length)return o}return null}function fo(t){let o=Object.keys(t);for(let n=0;n<o.length;n++)if(o[n].startsWith("__reactFiber$")||o[n].startsWith("__reactInternalInstance$"))return t[o[n]];return null}function vo(t){return!t||!t.type||typeof t.type=="string"?null:t.type.displayName||t.type.name||null}var gt,bt,xt;function yt(t){gt=t.captureScreenshot,bt=t.showCreatePopover,xt=t.positionTooltip}function mt(t,o){e.overlay.style.display="none",e.hoverOutline.style.display="none",e.componentTraceEl.style.display="none";var n=document.elementFromPoint(t,o);return e.overlay.style.display="",n&&ho(n)&&(n=null),n}function ho(t){for(;t;){if(t.className&&typeof t.className=="string"&&t.className.indexOf(s)!==-1)return!0;t=t.parentElement}return!1}function wt(){var t,o,n=!1;e.overlay.addEventListener("mousemove",function(a){if(e.activeMode==="select-element"){if(e.lockedEl)return;var r=mt(a.clientX,a.clientY);if(!r){e.hoverOutline.style.display="none",e.componentTraceEl.style.display="none",e.hoveredEl=null;return}e.hoveredEl=r;var d=r.getBoundingClientRect();e.hoverOutline.style.display="block",e.hoverOutline.style.top=d.top+window.scrollY+"px",e.hoverOutline.style.left=d.left+window.scrollX+"px",e.hoverOutline.style.width=d.width+"px",e.hoverOutline.style.height=d.height+"px";var l=Se(r);l&&l.length?(e.componentTraceEl.textContent=ue(l),e.componentTraceEl.style.display="block",xt(e.componentTraceEl,d)):e.componentTraceEl.style.display="none"}else if(e.activeMode==="screenshot"&&n){var c=Math.min(t,a.clientX),p=Math.min(o,a.clientY),f=Math.abs(a.clientX-t),v=Math.abs(a.clientY-o);e.screenshotRect.style.display="block",e.screenshotRect.style.left=c+window.scrollX+"px",e.screenshotRect.style.top=p+window.scrollY+"px",e.screenshotRect.style.width=f+"px",e.screenshotRect.style.height=v+"px"}}),e.overlay.addEventListener("mousedown",function(a){a.preventDefault(),a.stopPropagation(),e.activeMode==="screenshot"&&(n=!0,t=a.clientX,o=a.clientY,e.screenshotRect.style.display="none")}),e.overlay.addEventListener("mouseup",function(a){if(a.preventDefault(),a.stopPropagation(),e.activeMode==="screenshot"&&n){n=!1;var r=Math.min(t,a.clientX),d=Math.min(o,a.clientY),l=Math.abs(a.clientX-t),c=Math.abs(a.clientY-o);e.screenshotRect.style.display="none",l>10&&c>10&&gt(r,d,l,c)}}),e.overlay.addEventListener("click",function(a){if(a.preventDefault(),a.stopPropagation(),e.activeMode==="select-element"){var r=e.hoveredEl||mt(a.clientX,a.clientY);if(!r)return;var d=fe(r),l=pt(r),c=r.tagName.toLowerCase();if(r.className&&typeof r.className=="string"){var p=r.className.trim().split(/\s+/).filter(function(v){return!v.startsWith(s)});p.length&&(c+="."+p.slice(0,3).join("."))}var f=Se(r);bt(d,l,c,r,f)}})}g();x();function kt(){let t=0,o=0,n=0,a=0,r=!1,d=!1;e.fab.addEventListener("mousedown",function(l){l.button===0&&(r=!0,d=!1,t=l.clientX,o=l.clientY,n=e.fabCX,a=e.fabCY,l.preventDefault())}),document.addEventListener("mousemove",function(l){if(!r)return;let c=l.clientX-t,p=l.clientY-o;if(!d&&Math.abs(c)<4&&Math.abs(p)<4)return;d=!0;let f=n+c,v=a+p;f=Math.max(20+H,Math.min(window.innerWidth-20-H,f)),v=Math.max(20+H,Math.min(window.innerHeight-20-H,v)),ge(f,v,!1)}),document.addEventListener("mouseup",function(){r&&(r=!1,d&&(e.fab._wasDragged=!0,setTimeout(function(){e.fab._wasDragged=!1},300),Ct(e.fabCX,e.fabCY)))})}function ge(t,o,n){e.fabCX=t,e.fabCY=o;let a=t>window.innerWidth/2;e.toolbarContainer.style.transition=n?"all .2s ease":"none",e.toolbarContainer.style.top=o-20+"px",a?(e.toolbarContainer.style.left="auto",e.toolbarContainer.style.right=window.innerWidth-t-20+"px"):(e.toolbarContainer.style.right="auto",e.toolbarContainer.style.left=t-20+"px"),e.toolbarContainer.classList.toggle(s+"toolbar-right",a),e.toolbarContainer.classList.toggle(s+"toolbar-left",!a)}function Ct(t,o){try{sessionStorage.setItem("veld-fab-pos",JSON.stringify({x:t,y:o}))}catch{}}function Tt(){try{let t=sessionStorage.getItem("veld-fab-pos");if(t){let o=JSON.parse(t);ge(o.x,o.y,!1);return}}catch{}ge(20+H,window.innerHeight-20-H,!1)}function Be(){let t=e.fabCX,o=e.fabCY,n=!1,a=window.innerWidth-20-H,r=window.innerHeight-20-H,d=20+H;t>a&&(t=a,n=!0),t<d&&(t=d,n=!0),o>r&&(o=r,n=!0),o<d&&(o=d,n=!0),n&&(ge(t,o,!1),Ct(t,o))}be();g();k();x();Z();X();g();k();x();function T(){let t=e.threads.filter(o=>o.status==="open"&&z(o,e.lastSeenAt)).length;e.fabBadge.textContent=t?String(t):"",e.fabBadge.className=s+"badge"+(t?"":" "+s+"badge-hidden")}var Bt,ie,_t,Ht;function At(t){Bt=t.closeActivePopover,ie=t.renderAllPins,_t=t.addPin,Ht=t.scrollToThread}function Q(){e.panelOpen=!e.panelOpen,e.panelOpen&&(e.expandedThreadId=null),e.panel.classList.toggle(s+"panel-open",e.panelOpen),e.panelOpen&&E()}function mo(t){e.expandedThreadId=t,E()}function xe(){e.expandedThreadId=null,E()}function Ot(t){e.panelOpen=!0,e.panelTab="active",e.expandedThreadId=t,e.panel.classList.add(s+"panel-open"),E()}function go(){if(e.segBtnActive&&e.segBtnResolved){let t=e.threads.filter(function(n){return n.status==="open"}).length,o=e.threads.filter(function(n){return n.status==="resolved"}).length;e.segBtnActive.textContent="Active"+(t?" ("+t+")":""),e.segBtnResolved.textContent="Resolved"+(o?" ("+o+")":""),e.segBtnActive.className=s+"segmented-btn"+(e.panelTab==="active"?" "+s+"segmented-btn-active":""),e.segBtnResolved.className=s+"segmented-btn"+(e.panelTab==="resolved"?" "+s+"segmented-btn-active":"")}}function He(){if(!e.markReadBtn)return;let t=e.threads.some(function(o){return z(o,e.lastSeenAt)});e.markReadBtn.style.display=t?"":"none"}function E(){if(e.panelBody.innerHTML="",e.expandedThreadId){let o=S(e.threads,e.expandedThreadId);if(o){e.panelBackBtn.style.display="";let n=e.panelBackBtn.parentElement?.querySelector("."+s+"segmented");n&&(n.style.display="none"),e.markReadBtn&&(e.markReadBtn.style.display="none"),e.panelHeadTitle.textContent="Thread",bo(o);return}e.expandedThreadId=null}e.panelBackBtn.style.display="none";let t=e.panelBackBtn.parentElement?.querySelector("."+s+"segmented");t&&(t.style.display=""),e.panelHeadTitle.textContent="Threads",go(),He(),e.panelTab==="active"?xo():yo()}var St='<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 01-2-2V4a2 2 0 012-2h9a2 2 0 012 2v1"/></svg>';function _e(t,o,n){let a=i("div",n);a.appendChild(document.createTextNode(t+o));let r=i("span","panel-detail-copy-icon");return r.innerHTML=St,a.appendChild(r),a.addEventListener("click",function(d){d.stopPropagation(),navigator.clipboard.writeText(o).then(function(){r.innerHTML=b.check,setTimeout(function(){r.innerHTML=St},1500)})}),a}function bo(t){let o=i("div","panel-detail-header");o.appendChild(_e("ID: ",t.id.substring(0,20)+"\u2026","panel-detail-id"));let n=U(t),a;t.scope.type==="global"?a="Global":t.scope.type==="page"?(a="Page "+n,n==="/"&&(a+=" (home)")):(a="Page "+(n||"?"),n==="/"&&(a+=" (home)")),o.appendChild(i("div","panel-detail-title",a));let r=n&&n!==window.location.pathname;if(t.scope.type==="element"||r){let c=i("a","panel-detail-page-link",r?"Go to page \u2192":"Go to comment \u2192");c.href=n||"#",c.addEventListener("click",function(p){p.preventDefault(),Ht(t.id)}),o.appendChild(c)}if(t.component_trace&&t.component_trace.length&&o.appendChild(_e("",t.component_trace.join(" > "),"panel-detail-trace")),t.scope.type==="element"&&t.scope.selector&&o.appendChild(_e("",t.scope.selector,"panel-detail-selector")),e.panelBody.appendChild(o),t.status==="resolved"){let l=i("div","thread-messages-list");t.messages.forEach(function(f){let v=i("div","message message-"+f.author.type),u=i("span","message-author-icon");u.innerHTML=f.author.type==="agent"?b.robot:b.chat,v.appendChild(u);let h=i("div","message-body");h.appendChild(i("div","message-text",f.body));let w=f.author.type==="agent"?"Agent":"You";h.appendChild(i("div","message-meta",w+" \xB7 "+ve(f.created_at))),v.appendChild(h),l.appendChild(v)}),e.panelBody.appendChild(l);let c=i("div","thread-input-actions"),p=i("button","btn btn-primary btn-sm","Reopen Thread");p.addEventListener("click",function(){y("POST","/threads/"+t.id+"/reopen").then(function(){t.status="open",xe(),ie(),m("Thread reopened")})}),c.appendChild(p),e.panelBody.appendChild(c)}else e.panelBody.appendChild(ko(t))}function xo(){let t=e.threads.filter(function(a){return a.status==="open"});if(!t.length){e.panelBody.appendChild(i("div","panel-empty","No active threads."));return}let o={},n=[];t.forEach(function(a){let d=(U(a)||"/").split("?")[0];o[d]||(o[d]=[],n.push(d)),o[d].push(a)}),n.sort(function(a,r){return a===window.location.pathname?-1:r===window.location.pathname?1:a.localeCompare(r)}),n.forEach(function(a){let r="Page "+a;a==="/"&&(r+=" (home)"),wo(r,o[a])})}function yo(){let t=e.threads.filter(function(o){return o.status==="resolved"});if(!t.length){e.panelBody.appendChild(i("div","panel-empty","No resolved threads."));return}t.sort(function(o,n){return new Date(n.updated_at).getTime()-new Date(o.updated_at).getTime()}),t.forEach(function(o){e.panelBody.appendChild(Ft(o,!0))})}function wo(t,o){o.sort(function(a,r){return new Date(r.updated_at).getTime()-new Date(a.updated_at).getTime()});let n=i("div","panel-section");n.appendChild(i("div","panel-section-heading",t)),o.forEach(function(a){n.appendChild(Ft(a,!1))}),e.panelBody.appendChild(n)}function Ft(t,o){let n=i("div","thread-card"+(o?" thread-card-resolved":""));z(t,e.lastSeenAt)&&!o&&n.classList.add(s+"thread-card-unread"),n.dataset.threadId=t.id;let a=i("div","thread-card-row"),r=t.messages&&t.messages[0]?t.messages[0].body:"";r.length>50&&(r=r.substring(0,50)+"\u2026"),a.appendChild(i("span","thread-card-preview",r));let d=t.messages?t.messages.length:0,l=d>1?d+" replies":"";return l&&(l+=" \xB7 "),l+=ve(t.updated_at),a.appendChild(i("span","thread-card-meta",l)),n.appendChild(a),t.scope&&t.scope.type==="element"&&t.scope.selector&&n.appendChild(i("div","thread-card-selector",t.scope.selector)),n.addEventListener("click",function(){mo(t.id)}),n}function ko(t){let o=i("div","thread-messages"),n=i("div","thread-messages-list");t.messages.forEach(function(p){let f=i("div","message message-"+p.author.type),v=i("span","message-author-icon");v.innerHTML=p.author.type==="agent"?b.robot:b.chat,f.appendChild(v);let u=i("div","message-body");u.appendChild(i("div","message-text",p.body));let h=p.author.type==="agent"?"Agent":"You";u.appendChild(i("div","message-meta",h+" \xB7 "+ve(p.created_at))),f.appendChild(u),n.appendChild(f)}),o.appendChild(n),Co(t.id);let a=i("div","thread-input"),r=document.createElement("textarea");r.className=s+"textarea",r.placeholder="Reply...",r.rows=2,a.appendChild(r);let d=i("div","thread-input-actions"),l=i("button","btn btn-secondary btn-sm","Resolve \u2713");l.addEventListener("click",function(){let p=r.value.trim(),f=function(){y("POST","/threads/"+t.id+"/resolve").then(function(){t.status="resolved",Bt(),xe(),ie(),m("Thread resolved")})};p?y("POST","/threads/"+t.id+"/messages",{body:p}).then(function(v){t.messages.push(v),f()}):f()}),d.appendChild(l);let c=i("button","btn btn-primary btn-sm","Send"+V);return c.addEventListener("click",function(){let p=r.value.trim();p&&(c.disabled||(c.disabled=!0,y("POST","/threads/"+t.id+"/messages",{body:p}).then(function(f){t.messages.push(f),t.updated_at=new Date().toISOString(),r.value="",c.disabled=!1,e.panelOpen&&E(),ie()}).catch(function(){c.disabled=!1,m("Failed to send reply",!0)})))}),d.appendChild(c),$(r,c),a.appendChild(d),o.appendChild(a),o}function Co(t){e.lastSeenAt[t]=Date.now(),y("PUT","/threads/"+t+"/seen").catch(function(){});let o=S(e.threads,t);o&&_t(o),T(),He()}function It(){e.threads.forEach(function(t){z(t,e.lastSeenAt)&&(e.lastSeenAt[t.id]=Date.now(),y("PUT","/threads/"+t.id+"/seen").catch(function(){}))}),ie(),T(),He(),e.panelOpen&&E(),m("All marked as read")}g();x();Z();X();function ee(){e.listeningModule&&(e.listeningModule.style.display=e.agentListening?"flex":"none"),e.fab&&e.fab.classList.toggle(s+"fab-pulse",e.agentListening)}function Dt(){y("POST","/session/end").then(function(){m("All Good signal sent!"),e.agentListening=!1,ee()}).catch(function(t){m("Failed: "+t.message,!0)})}function Rt(){ht(),e.overlay=i("div","overlay"),document.body.appendChild(e.overlay),wt(),e.hoverOutline=i("div","hover-outline"),document.body.appendChild(e.hoverOutline),e.componentTraceEl=i("div","component-trace"),document.body.appendChild(e.componentTraceEl),e.toolbarContainer=i("div","toolbar-container"),e.toolbar=i("div","toolbar"),e.toolBtnSelect=j("select-element",b.crosshair,_("Select element",[F,R,"F"])),e.toolBtnScreenshot=j("screenshot",b.screenshot,_("Screenshot",[F,R,"S"])),e.toolBtnDraw=j("draw",b.draw,_("Draw",[F,R,"D"])),e.toolBtnPageComment=j("page-comment",b.pageComment,_("Page comment",[F,R,"P"])),e.toolBtnComments=j("show-comments",b.chat,_("Threads",[F,R,"C"])),e.toolbar.appendChild(e.toolBtnSelect),e.toolbar.appendChild(e.toolBtnScreenshot),e.toolbar.appendChild(e.toolBtnDraw),e.toolbar.appendChild(e.toolBtnPageComment),e.toolbar.appendChild(e.toolBtnComments),e.listeningModule=i("div","listening");let t=i("div","separator");e.listeningModule.appendChild(t);let o=i("span","listening-dot");N(o,"Agent is listening"),e.listeningModule.appendChild(o);let n=i("button","listening-allgood","All Good");n.addEventListener("click",function(h){h.stopPropagation(),Dt()}),e.listeningModule.appendChild(n),e.toolbar.appendChild(e.listeningModule),e.toolbar.appendChild(i("div","separator"));let a=i("button","tool-btn");a.innerHTML=b.keyboard,N(a,_("Disable shortcuts",[])),a.addEventListener("click",function(h){h.stopPropagation(),e.shortcutsDisabled=!e.shortcutsDisabled,a.classList.toggle(s+"tool-active",e.shortcutsDisabled),m(e.shortcutsDisabled?"Shortcuts disabled":"Shortcuts enabled")}),e.toolbar.appendChild(a);let r={auto:'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41"/></svg>',dark:'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12.79A9 9 0 1111.21 3 7 7 0 0021 12.79z"/></svg>',light:'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="5"/><line x1="12" y1="1" x2="12" y2="3"/><line x1="12" y1="21" x2="12" y2="23"/><line x1="4.22" y1="4.22" x2="5.64" y2="5.64"/><line x1="18.36" y1="18.36" x2="19.78" y2="19.78"/><line x1="1" y1="12" x2="3" y2="12"/><line x1="21" y1="12" x2="23" y2="12"/><line x1="4.22" y1="19.78" x2="5.64" y2="18.36"/><line x1="18.36" y1="5.64" x2="19.78" y2="4.22"/></svg>'},d={auto:"Auto (contrast)",dark:"Dark",light:"Light"},l=["auto","dark","light"],c=i("button","tool-btn");c.innerHTML=r[e.theme],N(c,_(d[e.theme],[])),c.addEventListener("click",function(h){h.stopPropagation();let w=(l.indexOf(e.theme)+1)%l.length;e.theme=l[w],c.innerHTML=r[e.theme],e.hostEl.setAttribute("data-theme",e.theme),m("Theme: "+d[e.theme])}),e.toolbar.appendChild(c);let p=i("button","tool-btn");p.innerHTML=b.dashboard,N(p,_("Management UI",[])),p.addEventListener("click",function(h){h.stopPropagation(),window.open("/__veld__/","_blank")}),e.toolbar.appendChild(p),e.toolBtnHide=j("hide",b.eyeOff,_("Hide",[F,R,"."])),e.toolbar.appendChild(e.toolBtnHide),e.screenshotRect=i("div","screenshot-rect"),document.body.appendChild(e.screenshotRect),e.toolbarContainer.appendChild(e.toolbar),e.fab=i("button","fab"),N(e.fab,_("Veld Feedback",[F,R,"V"])),e.fab.innerHTML=b.logo,e.fabBadge=i("span","badge badge-hidden"),e.fab.appendChild(e.fabBadge),e.fab.addEventListener("click",function(){if(e.fab._wasDragged){e.fab._wasDragged=!1;return}J()}),e.toolbarContainer.appendChild(e.fab),e.shadow.appendChild(e.toolbarContainer),kt(),e.panel=i("div","panel");let f=i("div","panel-head");e.panelBackBtn=i("button","panel-back-btn"),e.panelBackBtn.innerHTML='<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><polyline points="15 18 9 12 15 6"/></svg>',e.panelBackBtn.style.display="none",e.panelBackBtn.addEventListener("click",function(h){h.stopPropagation(),xe()}),f.appendChild(e.panelBackBtn),e.panelHeadTitle=i("span","panel-head-title","Threads"),f.appendChild(e.panelHeadTitle);let v=i("div","segmented");e.segBtnActive=i("button","segmented-btn segmented-btn-active","Active"),e.segBtnActive.addEventListener("click",function(){e.panelTab="active",E()}),e.segBtnResolved=i("button","segmented-btn","Resolved"),e.segBtnResolved.addEventListener("click",function(){e.panelTab="resolved",E()}),v.appendChild(e.segBtnActive),v.appendChild(e.segBtnResolved),f.appendChild(v),e.markReadBtn=i("button","panel-mark-read"),e.markReadBtn.innerHTML='<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="18 7 9.5 17 6 13"/><polyline points="22 7 13.5 17"/></svg>',e.markReadBtn.title="Mark all as read",e.markReadBtn.style.display="none",e.markReadBtn.addEventListener("click",function(h){h.stopPropagation(),It()}),f.appendChild(e.markReadBtn);let u=i("button","panel-close");u.innerHTML="&times;",u.addEventListener("click",Q),f.appendChild(u),e.panel.appendChild(f),e.panelBody=i("div","panel-body"),e.panel.appendChild(e.panelBody),e.shadow.appendChild(e.panel)}g();k();var te,q,zt,Nt,Xt,Ae,Ut;function Yt(t){te=t.setMode,q=t.toggleToolbar,zt=t.togglePageComment,Nt=t.togglePanel,Xt=t.hideOverlay,Ae=t.showOverlay,Ut=t.closeActivePopover}function jt(t){if(t.key==="Escape"&&e.activeMode==="draw"){t.preventDefault(),te(null);return}if(e.shortcutsDisabled)return;let o=Le(t)&&t.shiftKey;if(o&&t.code==="KeyV"){if(t.preventDefault(),e.hidden){Ae();return}q();return}if(o&&t.code==="Period"){t.preventDefault(),e.hidden?Ae():Xt();return}if(!e.hidden){if(o&&t.code==="KeyF"){t.preventDefault(),e.toolbarOpen||q(),te(e.activeMode==="select-element"?null:"select-element");return}if(o&&t.code==="KeyS"){t.preventDefault(),e.toolbarOpen||q(),te(e.activeMode==="screenshot"?null:"screenshot");return}if(o&&t.code==="KeyD"){t.preventDefault(),e.toolbarOpen||q(),te(e.activeMode==="draw"?null:"draw");return}if(o&&t.code==="KeyP"){t.preventDefault(),e.toolbarOpen||q(),zt();return}if(o&&t.code==="KeyC"){t.preventDefault(),Nt();return}t.key==="Escape"&&(e.activePopover?Ut():e.activeMode?te(null):e.toolbarOpen&&q())}}g();k();x();Z();X();k();var oe,qt,Kt,I,Fe,Ie,Gt;function Wt(t){oe=t.addPin,qt=t.removePin,Kt=t.renderAllPins,I=t.renderPanel,Fe=t.openThreadInPanel,Ie=t.scrollToThread,Gt=t.checkPendingScroll}function De(){y("GET","/events?after="+e.lastEventSeq).then(function(t){!t||!t.length||t.forEach(function(o){To(o),o.seq>e.lastEventSeq&&(e.lastEventSeq=o.seq)})}).catch(function(){})}function Re(){y("GET","/session").then(function(t){let o=e.agentListening;e.agentListening=t&&t.listening,e.agentListening!==o&&ee()}).catch(function(){})}function To(t){switch(t.event){case"agent_message":Eo(t);break;case"agent_thread_created":Mo(t);break;case"resolved":Po(t);break;case"reopened":Lo(t);break;case"agent_listening":e.agentListening=!0,ee();break;case"agent_stopped":e.agentListening=!1,ee(),m("Agent stopped listening");break;case"session_ended":e.agentListening=!1,ee();break;case"thread_created":t.thread&&!S(e.threads,t.thread.id)&&(e.threads.push(t.thread),oe(t.thread),T(),e.panelOpen&&I());break;case"human_message":if(t.thread_id&&t.message){let o=S(e.threads,t.thread_id);o&&(o.messages.some(function(a){return a.id===t.message.id})||(o.messages.push(t.message),o.updated_at=t.message.created_at||new Date().toISOString(),e.panelOpen&&I()))}break}}function Eo(t){let o=S(e.threads,t.thread_id);if(!o){y("GET","/threads/"+t.thread_id).then(function(a){a&&(e.threads.push(a),oe(a),T(),e.panelOpen&&I(),Oe(a.id,t.message.body))}).catch(function(){});return}if(t.message){let a=!1;for(let r=0;r<o.messages.length;r++)if(o.messages[r].id===t.message.id){a=!0;break}a||(o.messages.push(t.message),o.updated_at=t.message.created_at||new Date().toISOString())}oe(o),T(),e.panelOpen&&I();let n=t.message?t.message.body:"New reply";Oe(t.thread_id,n),document.hasFocus()||Vt("Agent replied",n,t.thread_id)}function Mo(t){if(t.thread){if(!S(e.threads,t.thread.id)){e.threads.push(t.thread),oe(t.thread),T(),e.panelOpen&&I();let n=t.thread.messages&&t.thread.messages[0]?t.thread.messages[0].body:"New thread";Oe(t.thread.id,n),document.hasFocus()||Vt("Agent started a thread",n,t.thread.id)}}else ze()}function Po(t){let o=S(e.threads,t.thread_id);o&&(o.status="resolved",qt(o.id),T(),e.panelOpen&&I())}function Lo(t){let o=S(e.threads,t.thread_id);o&&(o.status="open",oe(o),T(),e.panelOpen&&I())}function Oe(t,o){let n=i("div","agent-toast");n.appendChild(i("div","agent-toast-header","Agent replied"));let a=i("div","agent-toast-body");a.textContent=o.length>60?o.substring(0,60)+"...":o,n.appendChild(a);let r=i("button","agent-toast-link","Go to thread \u2192");r.addEventListener("click",function(){n.remove(),Fe(t),Ie(t)}),n.appendChild(r),e.shadow.appendChild(n),requestAnimationFrame(function(){n.classList.add(s+"agent-toast-show")}),setTimeout(function(){n.classList.remove(s+"agent-toast-show"),setTimeout(function(){n.remove()},300)},8e3)}function Vt(t,o,n){if(!("Notification"in window)||Notification.permission!=="granted")return;let a=new Notification(t,{body:o,icon:"/__veld__/feedback/logo.svg",tag:"veld-thread-"+n});a.addEventListener("click",function(){window.focus(),Fe(n),Ie(n),a.close()})}function ze(){y("GET","/threads").then(function(t){e.threads=t||[],Kt(),T(),Gt(),e.panelOpen&&I()}).catch(function(){})}Je();be();ye();g();x();var Ze,Qe;function no(t){Ze=t.setMode,Qe=t.togglePanel}function et(){e.hidden=!0;try{sessionStorage.setItem("veld-feedback-hidden","1")}catch{}e.toolbarContainer.classList.add(s+"hidden"),Object.keys(e.pins).forEach(t=>{e.pins[t].classList.add(s+"hidden")}),e.overlay.classList.remove(s+"overlay-active"),e.hoverOutline.style.display="none",e.componentTraceEl.style.display="none",Ze&&Ze(null),e.panelOpen&&Qe&&Qe()}function ao(){e.hidden=!1;try{sessionStorage.removeItem("veld-feedback-hidden")}catch{}e.toolbarContainer.classList.remove(s+"hidden"),Object.keys(e.pins).forEach(t=>{e.pins[t].classList.remove(s+"hidden")})}g();k();x();var ro=null;function se(t){if(t.status==="resolved")return;let o=U(t);if(!o||!he(o))return;let n=ft(t);if(!n)return;Ce(t.id);let a=i("div","pin");a.id=s+"pin-"+t.id,a.dataset.threadId=t.id;let r=i("span","pin-icon");r.innerHTML=b.chat,a.appendChild(r);let d=t.messages?t.messages.length:1;if(d>1){let l=i("span","pin-count",String(d));a.appendChild(l)}if(z(t,e.lastSeenAt)){let l=i("span","pin-unread-dot");a.appendChild(l)}a.style.position="absolute",a.style.top=n.y-12+"px",a.style.left=n.x+n.width-12+"px",a.style.zIndex="calc(var(--vf-z) - 1)",a.addEventListener("click",function(l){l.stopPropagation(),ro&&ro(t.id)}),document.body.appendChild(a),e.pins[t.id]=a}function Ce(t){e.pins[t]&&(e.pins[t].remove(),delete e.pins[t])}function Te(){Object.keys(e.pins).forEach(Ce),e.threads.forEach(function(t){t.status==="open"&&se(t)})}function Bo(){e.threads.forEach(function(t){let o=e.pins[t.id];if(o&&!(!t.scope||t.scope.type!=="element"||!t.scope.selector))try{let n=document.querySelector(t.scope.selector);if(n){let a=fe(n);t.scope.position={x:a.x,y:a.y,width:a.width,height:a.height},o.style.top=a.y-12+"px",o.style.left=a.x+a.width-12+"px"}}catch{}})}function tt(){e.rafPending||(e.rafPending=!0,requestAnimationFrame(function(){e.rafPending=!1,Bo()}))}g();k();x();var ot="veld-feedback-scroll-to-thread",nt,at;function io(t){nt=t.renderAllPins,at=t.renderPanel}function Ee(t){let o=S(e.threads,t);if(!o)return;let n=U(o);if(n&&!he(n)){try{sessionStorage.setItem(ot,t)}catch{}window.location.href=n;return}let a=null;if(o.scope&&o.scope.type==="element"&&o.scope.selector)try{a=document.querySelector(o.scope.selector)}catch{}if(a||(a=e.pins[t]||document.getElementById(s+"pin-"+t)),!a)return;a.scrollIntoView({behavior:"smooth",block:"center"});let r=e.pins[t];r&&setTimeout(()=>{r.classList.remove(s+"pin-highlight"),r.offsetWidth,r.classList.add(s+"pin-highlight"),setTimeout(()=>{r.classList.remove(s+"pin-highlight")},1500)},400)}function rt(){try{let t=sessionStorage.getItem(ot);t&&(sessionStorage.removeItem(ot),setTimeout(()=>Ee(t),300))}catch{}}function lo(){let t=window.location.pathname;t!==e.lastPathname&&(e.lastPathname=t,nt&&nt(),e.panelOpen&&at&&at(),rt())}ke();we();me();function _o(){Lt({setMode:W,togglePageComment:qe,togglePanel:Q,hideOverlay:et}),no({setMode:W,togglePanel:Q}),Yt({setMode:W,toggleToolbar:J,togglePageComment:qe,togglePanel:Q,hideOverlay:et,showOverlay:ao,closeActivePopover:M}),yt({captureScreenshot:to,showCreatePopover:je,positionTooltip:ut}),$t({addPin:se,updateBadge:T,renderPanel:E}),Wt({addPin:se,removePin:Ce,renderAllPins:Te,renderPanel:E,openThreadInPanel:Ot,scrollToThread:Ee,checkPendingScroll:rt}),At({closeActivePopover:M,renderAllPins:Te,addPin:se,scrollToThread:Ee}),Ke({setMode:W}),eo({setMode:W}),io({renderAllPins:Te,renderPanel:E})}function it(){try{sessionStorage.getItem("veld-hidden")==="1"&&(e.hidden=!0)}catch{}_o(),Rt(),Tt(),Be(),e.hidden&&e.toolbarContainer.classList.add(s+"hidden"),document.addEventListener("keydown",jt,!0),window.addEventListener("scroll",tt,!0),window.addEventListener("resize",()=>{tt(),Be()}),window.addEventListener("popstate",lo),ze(),De(),Re(),setInterval(De,3e3),setInterval(Re,5e3),"Notification"in window&&Notification.permission==="default"&&Notification.requestPermission()}if(!window.__veld_feedback_initialised){window.__veld_feedback_initialised=!0;let t=document.createElement("veld-feedback");t.style.cssText="display:contents",document.body.appendChild(t);let o=t.attachShadow({mode:"open"}),n=document.createElement("style");n.textContent=Pe,o.appendChild(n);let a=document.createElement("style");a.textContent=st,a.setAttribute("data-veld","light"),(document.head||document.documentElement).appendChild(a),ct(o,t),document.readyState==="loading"?document.addEventListener("DOMContentLoaded",it):it()}})();
