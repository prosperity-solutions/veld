// Veld client-side log collector.
// Captures console output and unhandled errors, posts them to veld-daemon.
// Injected automatically by Caddy into HTML responses for veld services.
(function(){
'use strict';
try{
// Dedup guard — if injected via both <head> and <body> fallback, only run once.
if(window.__veld_cl)return;
window.__veld_cl=1;
// Top-frame only — avoid double-collection in iframes.
if(window!==window.top)return;

var sc=document.currentScript;
var levelsAttr=sc&&sc.getAttribute('data-veld-levels');
var levels=levelsAttr?levelsAttr.split(','):([]); // empty = capture nothing except exceptions
var levelSet={};
for(var i=0;i<levels.length;i++)levelSet[levels[i]]=1;

var buf=[];
var timer=null;
var endpoint='/__veld__/api/client-logs';
var MAX_BUF=50;
var FLUSH_MS=1000;

function flush(){
  if(!buf.length)return;
  var batch=buf;buf=[];
  try{
    var x=new XMLHttpRequest();
    x.open('POST',endpoint,true);
    x.setRequestHeader('Content-Type','application/json');
    x.send(JSON.stringify({entries:batch}));
  }catch(e){}
}

function schedule(){
  if(timer)return;
  timer=setTimeout(function(){timer=null;flush();},FLUSH_MS);
}

function push(entry){
  buf.push(entry);
  if(buf.length>=MAX_BUF){if(timer){clearTimeout(timer);timer=null;}flush();}
  else schedule();
}

function now(){return new Date().toISOString();}

var MAX_ARG_LEN=8192; // 8KB per argument, prevent huge payloads
function truncate(s){return s.length>MAX_ARG_LEN?s.substring(0,MAX_ARG_LEN)+'...(truncated)':s;}
function stringify(args){
  var parts=[];
  for(var i=0;i<args.length;i++){
    var a=args[i];
    if(a===null)parts.push('null');
    else if(a===undefined)parts.push('undefined');
    else if(typeof a==='object'){try{parts.push(truncate(JSON.stringify(a)));}catch(e){parts.push(String(a));}}
    else parts.push(truncate(String(a)));
  }
  return parts.join(' ');
}

function captureStack(){
  try{var e=new Error();return e.stack||'';}catch(x){return '';}
}

var needsStack={'error':1,'warn':1};

// Monkey-patch console methods.
var con=window.console;
var methods=['log','warn','error','info','debug'];
for(var m=0;m<methods.length;m++){
  (function(name){
    if(!levelSet[name])return; // skip if not in configured levels
    var orig=con[name];
    if(typeof orig!=='function')return;
    con[name]=function(){
      // Always call original first.
      orig.apply(con,arguments);
      try{
        var entry={ts:now(),level:name,msg:stringify(arguments)};
        if(needsStack[name])entry.stack=captureStack();
        push(entry);
      }catch(e){}
    };
  })(methods[m]);
}

// Capture unhandled exceptions — always, regardless of level config.
window.addEventListener('error',function(ev){
  try{
    push({
      ts:now(),
      level:'exception',
      msg:ev.message||String(ev),
      stack:ev.error&&ev.error.stack?ev.error.stack:(ev.filename?ev.filename+':'+ev.lineno+':'+ev.colno:'')
    });
  }catch(e){}
});

// Capture unhandled promise rejections — always.
window.addEventListener('unhandledrejection',function(ev){
  try{
    var reason=ev.reason;
    var msg=reason instanceof Error?reason.message:String(reason||'');
    var stack=reason instanceof Error&&reason.stack?reason.stack:'';
    push({ts:now(),level:'exception',msg:'Unhandled Promise rejection: '+msg,stack:stack});
  }catch(e){}
});

// Flush on page unload using sendBeacon (survives page navigation).
window.addEventListener('beforeunload',function(){
  if(!buf.length)return;
  var data=JSON.stringify({entries:buf});buf=[];
  if(navigator.sendBeacon){
    navigator.sendBeacon(endpoint,new Blob([data],{type:'application/json'}));
  }else{
    try{var x=new XMLHttpRequest();x.open('POST',endpoint,false);x.setRequestHeader('Content-Type','application/json');x.send(data);}catch(e){}
  }
});

}catch(e){}
})();
