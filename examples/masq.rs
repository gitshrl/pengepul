use std::fs;
fn main(){
  let raw=fs::read_to_string("/tmp/prod-body.json").unwrap();
  let b:serde_json::Value=serde_json::from_str(&raw).unwrap();
  let (out,_)=pengepul::masquerade::masquerade_request(&b);
  fs::write("/tmp/masq-out.json",serde_json::to_string(&out).unwrap()).unwrap();
  let sys=out["system"][0]["text"].as_str().unwrap_or("");
  let tools:Vec<&str>=out["tools"].as_array().unwrap().iter().filter_map(|t|t["name"].as_str()).collect();
  eprintln!("masqueraded: tools={:?}", &tools[..tools.len().min(8)]);
  eprintln!("openclaw tool names leaked in sys? exec={} gateway={} nodes={}", sys.contains(" exec"), sys.contains("gateway"), sys.contains("nodes"));
}
