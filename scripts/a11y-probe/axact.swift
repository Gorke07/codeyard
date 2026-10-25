import Cocoa
import ApplicationServices
let pid = pid_t(CommandLine.arguments[1])!
func attr(_ e: AXUIElement, _ a: String) -> AnyObject? {
  var v: CFTypeRef?
  return AXUIElementCopyAttributeValue(e, a as CFString, &v) == .success ? v : nil
}
func find(_ e: AXUIElement, _ role: String, _ title: String? = nil) -> AXUIElement? {
  if (attr(e, "AXRole") as? String) == role && (title == nil || (attr(e, "AXTitle") as? String) == title) { return e }
  for k in (attr(e, "AXChildren") as? [AXUIElement] ?? []) { if let r = find(k, role, title) { return r } }
  return nil
}
func strForRange(_ e: AXUIElement, _ loc: Int, _ len: Int) -> String {
  var rg = CFRange(location: loc, length: len)
  let v = AXValueCreate(.cfRange, &rg)!
  var out: CFTypeRef?
  let err = AXUIElementCopyParameterizedAttributeValue(e, "AXStringForRange" as CFString, v, &out)
  return err == .success ? "\(out!)".debugDescription : "err \(err.rawValue)"
}
func report(_ ta: AXUIElement, _ tag: String) {
  var rg = CFRange()
  if let r = attr(ta, "AXSelectedTextRange") { AXValueGetValue(r as! AXValue, .cfRange, &rg) }
  print("[\(tag)] title=\(attr(ta,"AXTitle") ?? "-" as AnyObject) chars=\(attr(ta,"AXNumberOfCharacters") ?? 0 as AnyObject) sel=\(rg.location)+\(rg.length) selText=\((attr(ta,"AXSelectedText") as? String ?? "").debugDescription) value=\(("\(attr(ta,"AXValue") ?? "nil" as AnyObject)").debugDescription)")
  print("   StringForRange(0,9)=\(strForRange(ta, 0, 9))  StringForRange(16,12)=\(strForRange(ta,16,12))")
}
let app = AXUIElementCreateApplication(pid)
let ta = find(app, "AXTextArea")!
report(ta, "initial")
var rg = CFRange(location: 20, length: 7)   // println
let v = AXValueCreate(.cfRange, &rg)!
let e = AXUIElementSetAttributeValue(ta, "AXSelectedTextRange" as CFString, v)
print("set AXSelectedTextRange 20+7 ->", e.rawValue)
Thread.sleep(forTimeInterval: 0.8)
report(find(app, "AXTextArea")!, "after set selection")
let row = find(app, "AXRow", "main.rs")!
print("rows actions:", { var a: CFArray?; AXUIElementCopyActionNames(row, &a); return a as? [String] ?? [] }())
let pe = AXUIElementPerformAction(row, kAXPressAction as CFString)
print("AXPress main.rs ->", pe.rawValue)
Thread.sleep(forTimeInterval: 1.0)
report(find(app, "AXTextArea")!, "after pressing main.rs row")
if let o = find(app, "AXOutline") {
  for r in (attr(o, "AXChildren") as? [AXUIElement] ?? []) { print("row", attr(r,"AXTitle") ?? "-" as AnyObject, "selected=", attr(r,"AXSelected") ?? "-" as AnyObject) }
}
