//! (v261) Les scripts des pages, pour Edge (WebView2). Les mêmes que ceux de l'ancien
//! navigateur Chromium (`crates/browser/src/main.rs`), avec deux différences :
//!
//! - ils parlent au navigateur par `window.chrome.webview.postMessage` (Edge ne donne pas
//!   les messages de la console) ;
//! - Edge les pose au tout début de chaque document **et de chaque cadre** : ceux de la page
//!   ne font rien dans un cadre, et celui des pubs ne fait rien ailleurs que dans le cadre du
//!   lecteur officiel, d'où il parle à la page du lecteur (qui relaie au jeu).

/// Signale le focus d'un champ de texte, l'état de la page (vidéo, temps, pause, pub),
/// renvoie les vidéos de YouTube au lecteur (en ligne), garde les Shorts jusqu'au regard,
/// et dit les vidéos cliquées (file d'attente). Voir le navigateur Chromium pour l'histoire.
pub const PAGE_JS: &str = r#"(()=>{if(window.top!==window||window.__minelib)return;window.__minelib=1;
const dit=s=>{try{window.chrome.webview.postMessage(s);}catch(e){}};

const ed=e=>!!e&&(e.isContentEditable||e.tagName==='TEXTAREA'||(e.tagName==='INPUT'&&!/^(button|checkbox|color|file|hidden|image|radio|range|reset|submit)$/i.test(e.type||'')));
const deep=()=>{let a=document.activeElement;while(a&&a.shadowRoot&&a.shadowRoot.activeElement)a=a.shadowRoot.activeElement;return a;};
let f=null;const focus=()=>{const v=ed(deep())?1:0;if(v!==f){f=v;dit('__minelib_focus__'+v);}};
document.addEventListener('focusin',focus,true);
document.addEventListener('focusout',()=>setTimeout(focus,0),true);
document.addEventListener('mousedown',()=>setTimeout(()=>{f=null;focus();},0),true);
const L='https://viendax.github.io/minelib/lecteur';
const SS=k=>{try{return sessionStorage.getItem(k);}catch(e){return null;}};
const vid=u=>{try{const x=new URL(u,location.href);const h=x.hostname.toLowerCase();let i='';
if(h==='youtu.be')i=x.pathname.split('/')[1]||'';
else if(h==='www.youtube.com'||h==='youtube.com'||h==='m.youtube.com'){
if(x.pathname==='/watch')i=x.searchParams.get('v')||'';
else{const m=x.pathname.match(/^\/live\/([^\/?#]+)/);if(m)i=m[1];}}
if(!/^[A-Za-z0-9_-]{11}$/.test(i))return '';
if(x.searchParams.get('mlb')==='site'){try{sessionStorage.setItem('mlb_site_'+i,'1');}catch(e){}return '';}
if(SS('mlb_site_'+i))return '';
const t=parseInt(x.searchParams.get('t')||x.searchParams.get('start')||'0',10)||0;
return L+'?v='+i+(t>0?'&t='+t:'');}catch(e){return '';}};
const yt=/(^|\.)youtube\.com$/.test(location.hostname);
const SH=()=>location.pathname.startsWith('/shorts/');
const gate=yt&&!window.__mlb_site;let ok=false;const SV=()=>document.querySelector('#shorts-player video');
const attend=()=>gate&&SH()&&!ok;
const PUBS='ytd-ad-slot-renderer,ytd-in-feed-ad-layout-renderer,ad-badge-view-model,ytd-ad-inline-playback-meta-block,[class*="ad-badge"]';
const pubShort=()=>{if(!SH())return false;const c=document.elementFromPoint(innerWidth/2,innerHeight/2);
const r=c&&c.closest&&c.closest('ytd-reel-video-renderer');return !!r&&(!!r.querySelector(PUBS)||/Sponsoris|Sponsored/.test(r.innerText||''));};
if(gate){window.__mlb_short=()=>{if(ok)return;ok=true;const v=SV();if(v&&v.paused)v.play().catch(()=>{});};
document.addEventListener('play',e=>{if(attend()&&e.target&&e.target.pause)e.target.pause();},true);
setInterval(()=>{const v=SV();if(attend()&&v&&!v.paused)v.pause();},250);}
const num=u=>{try{const x=new URL(u,location.href);const h=x.hostname.toLowerCase();let i='';
if(h==='youtu.be')i=x.pathname.split('/')[1]||'';
else if(/(^|\.)youtube\.com$/.test(h)){if(x.pathname==='/watch')i=x.searchParams.get('v')||'';
else{const m=x.pathname.match(/^\/(live|shorts)\/([^\/?#]+)/);if(m)i=m[2];}}
return /^[A-Za-z0-9_-]{11}$/.test(i)?i:'';}catch(e){return '';}};
const ordre=o=>dit('__minelib_ordre__'+JSON.stringify(o));
if(yt&&window.__mlb_cote){let dit='';
document.addEventListener('click',e=>{const a=e.composedPath().find(x=>x&&x.tagName==='A'&&x.href);const i=a&&num(a.href);
if(i){e.preventDefault();e.stopImmediatePropagation();ordre({clic:i});}},true);
setInterval(()=>{const i=num(location.href);if(!i){dit='';return;}if(i===dit)return;dit=i;ordre({clic:i});
if(history.length>1)history.back();else location.replace('https://www.youtube.com/');},250);}
else if(yt&&!window.__mlb_site){
const go=(u,r)=>{const l=vid(u);if(l){if(r)location.replace(l);else location.href=l;return true;}return false;};
document.addEventListener('click',e=>{const a=e.composedPath().find(x=>x&&x.tagName==='A'&&x.href);
if(a&&vid(a.href)){e.preventDefault();e.stopImmediatePropagation();
dit('__minelib_ordre__'+JSON.stringify({clic:new URL(vid(a.href)).searchParams.get('v')}));}},true);
setInterval(()=>go(location.href,true),250);go(location.href,true);}
let s='';window.addEventListener('pageshow',()=>{s='';});setInterval(()=>{focus();if(window.__mlb_lecteur)return;const mp=document.getElementById(SH()?'shorts-player':'movie_player');
const v=(mp&&mp.querySelector('video'))||document.querySelector('video');
let i='';try{if(mp&&mp.getVideoData)i=mp.getVideoData().video_id||'';}catch(e){}
const st=JSON.stringify({u:location.href,t:v?Math.round(v.currentTime*10)/10:-1,p:v?v.paused:true,a:!!document.querySelector('.ad-showing')||attend()||pubShort(),i:i,e:!!(v&&v.ended)});
if(st!==s){s=st;dit('__minelib_state__'+st);}},500);})();"#;

/// Dit si la page peut défiler sous la souris (la molette ne va à l'écran que si elle y
/// sert).
pub const SCROLL_JS: &str = r#"(()=>{if(window.top!==window||window.__minelib_scroll)return;window.__minelib_scroll=1;
const dit=s=>{try{window.chrome.webview.postMessage(s);}catch(e){}};
let mx=-1,my=-1,w=-1;
const sf=()=>{let f=0;const d=document.scrollingElement||document.documentElement;
if(d){if(d.scrollTop>0)f|=1;if(d.scrollTop+window.innerHeight<d.scrollHeight-1)f|=2;}
let e=mx<0?null:document.elementFromPoint(mx,my);
while(e&&e.shadowRoot){const i=e.shadowRoot.elementFromPoint(mx,my);if(!i||i===e)break;e=i;}
for(;e&&e!==d;e=e.parentElement||(e.getRootNode&&e.getRootNode().host)||null){
if(e.tagName==='IFRAME'){f|=3;break;}
const o=getComputedStyle(e).overflowY;
if((o==='auto'||o==='scroll'||o==='overlay')&&e.scrollHeight>e.clientHeight+1){
if(e.scrollTop>0)f|=1;if(e.scrollTop+e.clientHeight<e.scrollHeight-1)f|=2;}}
return f;};
const sr=()=>{const v=window.__mlb_lecteur?0:sf();if(v!==w){w=v;dit('__minelib_scroll__'+v);}};
document.addEventListener('mousemove',ev=>{mx=ev.clientX;my=ev.clientY;sr();},true);
document.addEventListener('scroll',()=>setTimeout(sr,0),true);
setInterval(sr,500);sr();})();"#;

/// Dans le cadre du lecteur officiel (`youtube-nocookie.com/embed`) : dit seulement si une
/// publicité y joue, et ce que voit le lecteur (gels, sauts). Il lit, il ne touche à rien.
pub const PUB_JS: &str = r#"(()=>{if(window.__minelib_pub||location.hostname!=='www.youtube-nocookie.com'||!location.pathname.startsWith('/embed/')||window.top===window)return;window.__minelib_pub=1;
const dit=s=>{try{window.parent.postMessage({__mlb:s},'https://viendax.github.io');}catch(e){}};
let x=-1;
setInterval(()=>{const a=document.querySelector('.ad-showing')?1:0;
if(a!==x){x=a;dit('__minelib_pub__'+a);}},400);
let v=null,mt=-1,rt=0,gel=0,perdues=-1;const D=m=>dit('__minelib_diag__'+m);
const suit=e=>{if(!e||e===v)return;v=e;['waiting','stalled','seeking','emptied','pause','play'].forEach(n=>e.addEventListener(n,()=>D(n+' a '+e.currentTime.toFixed(2))));};
setInterval(()=>{suit(document.querySelector('video'));if(!v)return;const now=performance.now();
if(v.paused||v.ended||v.readyState<3){mt=-1;gel=0;return;}
const t=v.currentTime;if(mt>=0){const dr=now-rt,dm=(t-mt)*1000/(v.playbackRate||1);
if(dm<dr*0.5&&dr>0){gel+=dr;}else{if(gel>=120)D('gel '+Math.round(gel)+' ms a '+t.toFixed(2));gel=0;}
if(dm>dr+250)D('saut '+Math.round(dm-dr)+' ms a '+t.toFixed(2));}
mt=t;rt=now;
const q=v.getVideoPlaybackQuality&&v.getVideoPlaybackQuality();if(q){const p=q.droppedVideoFrames;
if(perdues>=0&&p-perdues>=6)D('images perdues '+(p-perdues));perdues=p;}},100);})();"#;

/// Le champ de texte actif perd le focus.
pub const BLUR_JS: &str = "(()=>{let a=document.activeElement;\
while(a&&a.shadowRoot&&a.shadowRoot.activeElement)a=a.shadowRoot.activeElement;\
if(a&&a.blur)a.blur();})()";

/// La page du lecteur des écrans, servie par le navigateur à `filtre::LECTEUR`.
pub const LECTEUR_HTML: &str = include_str!("../../../browser/src/lecteur.html");
