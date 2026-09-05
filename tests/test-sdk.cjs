const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const source = fs.readFileSync(__dirname + '/../payload/lib/nethos.js', 'utf8');

async function run(native, iframe) {
  let streams = 0, reloads = 0;
  const classes = new Set();
  const root = {dataset:{},style:{setProperty(){}},classList:{
    remove(...names){names.forEach(n=>classes.delete(n));},
    toggle(name,on){if(on)classes.add(name);else classes.delete(name);}
  }};
  const window = {location:{pathname:'/apps/test/index.html',reload(){reloads++;}},
    matchMedia:()=>({matches:false})};
  window.self=window;window.top=iframe?{}:window;
  if(native)window.nethosHost={};
  const context={window,document:{documentElement:root},console,
    fetch:async()=>({ok:true,status:200,json:async()=>({settings:{theme:'dark'},generation:1})}),
    EventSource:class {constructor(){streams++;}},setTimeout};
  vm.runInNewContext(source,context);
  await window.nethos.ready();
  assert.equal(streams,native||iframe?0:1);
  if(native||iframe) {
    window.nethosEvent({type:'settings',generation:1,data:{theme:'auto',effective_theme:'light',reduced_transparency:true,animations:false}});
    assert.equal(root.dataset.theme,'light');
    assert(classes.has('reduced-transparency'));
    assert(classes.has('no-motion'));
    assert.equal(reloads,0,'appearance must not discard app state');
    window.nethos.autoReload(false);
    window.nethosEvent({type:'reload',generation:2});
    assert.equal(reloads,0);
  }
}
(async()=>{await run(true,false);await run(false,true);await run(false,false);console.log('PASS: host/iframe event sharing, browser fallback, live appearance and unsaved-state opt-out');})();
