import Cocoa
import ApplicationServices
let pid = pid_t(CommandLine.arguments[1])!
let target = CommandLine.arguments[2]
func attr(_ e: AXUIElement, _ a: String) -> AnyObject? {
  var v: CFTypeRef?
  return AXUIElementCopyAttributeValue(e, a as CFString, &v) == .success ? v : nil
}
func find(_ e: AXUIElement, _ role: String, _ title: String? = nil) -> AXUIElement? {
  if (attr(e, "AXRole") as? String) == role && (title == nil || (attr(e, "AXTitle") as? String) == title) { return e }
  for k in (attr(e, "AXChildren") as? [AXUIElement] ?? []) { if let r = find(k, role, title) { return r } }
  return nil
}
func show(_ tag: String) {
  let ta = find(app, "AXTextArea")
  print("[\(tag)] editor title=\(ta.flatMap { attr($0, "AXTitle") } ?? "none" as AnyObject) value=\(("\(ta.flatMap { attr($0, "AXValue") } ?? "none" as AnyObject)").debugDescription)")
  if let st = find(app, "AXGroup", "Status bar") {
    print("   status:", (attr(st, "AXChildren") as? [AXUIElement] ?? []).map { "\(attr($0, "AXValue") ?? "?" as AnyObject)" })
  }
  if let o = find(app, "AXOutline") {
    print("   rows:", (attr(o, "AXChildren") as? [AXUIElement] ?? []).map { "\(attr($0,"AXTitle") ?? "-" as AnyObject)[sel=\(attr($0,"AXSelected") ?? "-" as AnyObject)]" })
  }
}
let app = AXUIElementCreateApplication(pid)
show("before")
let row = find(app, "AXRow", target)!
print("AXPress \(target) ->", AXUIElementPerformAction(row, kAXPressAction as CFString).rawValue)
Thread.sleep(forTimeInterval: 1.2)
show("after")
