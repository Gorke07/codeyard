import Cocoa
import ApplicationServices
let pid = pid_t(CommandLine.arguments[1])!
print("AXIsProcessTrusted:", AXIsProcessTrusted())
func attr(_ e: AXUIElement, _ a: String) -> AnyObject? {
  var v: CFTypeRef?
  return AXUIElementCopyAttributeValue(e, a as CFString, &v) == .success ? v : nil
}
func dump(_ e: AXUIElement, _ d: Int, _ maxd: Int) {
  let role = attr(e, "AXRole") as? String ?? "?"
  var parts = ["role=\(role)"]
  for k in ["AXSubrole","AXTitle","AXDescription","AXValue","AXSelected","AXExpanded","AXFocused","AXNumberOfCharacters","AXSelectedText","AXLevel"] {
    if let v = attr(e,k) { var s = "\(v)"; if s.count > 90 { s = String(s.prefix(90)) + "…" }; parts.append("\(k.dropFirst(2))=\(s.debugDescription)") }
  }
  if let r = attr(e, "AXSelectedTextRange") { var rg = CFRange(); AXValueGetValue(r as! AXValue, .cfRange, &rg); parts.append("SelRange=\(rg.location)+\(rg.length)") }
  print(String(repeating: "  ", count: d) + parts.joined(separator: " "))
  if d >= maxd { return }
  let kids = attr(e, "AXChildren") as? [AXUIElement] ?? []
  for (i,k) in kids.enumerated() {
    if i >= 14 && (role == "AXOutline" || role == "AXList") { print(String(repeating: "  ", count: d+1) + "... (\(kids.count) children)"); break }
    dump(k, d+1, maxd)
  }
}
let app = AXUIElementCreateApplication(pid)
if let f = attr(app, "AXFocusedUIElement") { print("FOCUSED:"); dump(f as! AXUIElement, 1, 0) }
dump(app, 0, 12)
