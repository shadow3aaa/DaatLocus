import AppKit
import ApplicationServices
import ScreenCaptureKit

private struct Failure: Error { let message: String }
private func require(_ condition: Bool, _ message: String) throws {
  if !condition { throw Failure(message: message) }
}
private func attribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
  var value: CFTypeRef?
  return AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success ? value : nil
}
private func text(_ element: AXUIElement, _ name: String) -> String {
  attribute(element, name) as? String ?? ""
}
private func children(_ element: AXUIElement) -> [AXUIElement] {
  attribute(element, kAXChildrenAttribute) as? [AXUIElement] ?? []
}
private func frame(_ element: AXUIElement) -> CGRect? {
  guard let p = attribute(element, kAXPositionAttribute),
    let s = attribute(element, kAXSizeAttribute), CFGetTypeID(p) == AXValueGetTypeID(),
    CFGetTypeID(s) == AXValueGetTypeID()
  else { return nil }
  var point = CGPoint.zero
  var size = CGSize.zero
  guard AXValueGetValue(p as! AXValue, .cgPoint, &point),
    AXValueGetValue(s as! AXValue, .cgSize, &size)
  else { return nil }
  return CGRect(origin: point, size: size)
}
private func writable(_ element: AXUIElement, _ name: String) -> Bool {
  var value = DarwinBoolean(false)
  return AXUIElementIsAttributeSettable(element, name as CFString, &value) == .success
    && value.boolValue
}
private func number(_ request: [String: Any], _ key: String) throws -> Double {
  guard let value = request[key] as? NSNumber, CFGetTypeID(value) != CFBooleanGetTypeID(),
    value.doubleValue.isFinite
  else { throw Failure(message: "Invalid " + key) }
  return value.doubleValue
}
private func string(_ request: [String: Any], _ key: String) throws -> String {
  guard let value = request[key] as? String else { throw Failure(message: "Invalid " + key) }
  return value
}
private func set(_ element: AXUIElement, _ name: String, _ value: CFTypeRef) throws {
  try requireAdmission()
  effect = true
  try require(
    AXUIElementSetAttributeValue(element, name as CFString, value) == .success,
    "Target does not accept this operation")
}
private func actions(_ element: AXUIElement) -> [String] {
  var result: CFArray?
  AXUIElementCopyActionNames(element, &result)
  return result as? [String] ?? []
}
private func rectJSON(_ r: CGRect) -> [String: Any] {
  ["x": r.minX, "y": r.minY, "width": r.width, "height": r.height]
}
private func readRect(_ value: Any?) throws -> CGRect {
  // In-process CoreGraphics values are CGFloat, while JSON decodes NSNumber.
  // Read both through the same numeric validation rather than a Double cast.
  guard let rectangle = value as? [String: Any] else {
    throw Failure(message: "Invalid rectangle")
  }
  return try CGRect(
    x: number(rectangle, "x"), y: number(rectangle, "y"),
    width: number(rectangle, "width"), height: number(rectangle, "height"))
}
private func excluded(_ app: NSRunningApplication) -> Bool {
  let identifier = app.bundleIdentifier ?? ""
  return app.processIdentifier == getpid() || identifier.hasPrefix("io.daat-locus.")
    || app.executableURL?.lastPathComponent.hasPrefix("daat-locus") == true
}
private func app(_ pid: pid_t) throws -> NSRunningApplication {
  guard let value = NSRunningApplication(processIdentifier: pid), !value.isTerminated,
    !excluded(value), value.launchDate != nil
  else { throw Failure(message: "Target process unavailable or excluded") }
  return value
}
private func windows() -> [[String: Any]] {
  let list =
    CGWindowListCopyWindowInfo([.optionAll, .excludeDesktopElements], kCGNullWindowID)
    as? [[String: Any]] ?? []
  return list.filter {
    ($0[kCGWindowLayer as String] as? Int) == 0
      && ($0[kCGWindowOwnerPID as String] as? Int) != Int(getpid())
  }
}
private func info(_ id: UInt32) throws -> [String: Any] {
  guard let w = windows().first(where: { ($0[kCGWindowNumber as String] as? UInt32) == id }),
    let pid = w[kCGWindowOwnerPID as String] as? Int32,
    let dictionary = w[kCGWindowBounds as String] as? [String: Any],
    let bounds = CGRect(dictionaryRepresentation: dictionary as CFDictionary)
  else { throw Failure(message: "Window no longer exists") }
  let application = try app(pid)
  try require(bounds.width > 0 && bounds.height > 0, "Window has no renderable area")
  return [
    "id": id, "pid": pid, "started": application.launchDate!.timeIntervalSince1970,
    "bundle": application.bundleIdentifier ?? "", "path": application.bundleURL?.path ?? "",
    "name": application.localizedName ?? "", "title": w[kCGWindowName as String] as? String ?? "",
    "bounds": rectJSON(bounds),
  ]
}
private typealias WindowIDFunction =
  @convention(c) (AXUIElement, UnsafeMutablePointer<CGWindowID>) -> AXError
private let windowIDFunction: WindowIDFunction? = {
  guard let symbol = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "_AXUIElementGetWindow") else {
    return nil
  }
  return unsafeBitCast(symbol, to: WindowIDFunction.self)
}()
private func windowID(_ element: AXUIElement) -> UInt32? {
  var id: CGWindowID = 0
  return windowIDFunction?(element, &id) == .success && id != 0 ? id : nil
}
private final class Lease {
  let id: UInt32
  let pid: pid_t
  let started: Double
  let element: AXUIElement
  var receiver: AXUIElement?
  var snapshot: Data?
  func controls() -> [[String: Any]] {
    var result: [[String: Any]] = []
    var budget = 512
    let deadline = Date().addingTimeInterval(2)
    func visit(_ node: AXUIElement, _ depth: Int) {
      guard budget > 0, depth < 24, Date() < deadline, result.count < 256 else { return }
      budget -= 1
      let role = text(node, kAXRoleAttribute)
      if let bounds = frame(node), bounds.width > 0, bounds.height > 0 {
        var item: [String: Any] = [
          "role": role, "identifier": text(node, kAXIdentifierAttribute),
          "title": text(node, kAXTitleAttribute), "bounds": rectJSON(bounds),
          "actions": actions(node),
        ]
        if text(node, kAXSubroleAttribute) != kAXSecureTextFieldSubrole {
          if let value = attribute(node, kAXValueAttribute) as? String, value.utf8.count <= 4096 {
            item["value"] = value
          } else if let value = attribute(node, kAXValueAttribute) as? NSNumber {
            item["value"] = value
          }
        }
        result.append(item)
      }
      for child in children(node) { visit(child, depth + 1) }
    }
    visit(element, 0)
    return result
  }
  func signature(_ controls: [[String: Any]]) throws -> Data {
    try JSONSerialization.data(withJSONObject: controls, options: [.sortedKeys])
  }
  func matching(_ point: CGPoint, _ predicate: (AXUIElement) -> Bool) -> AXUIElement? {
    var budget = 512
    let deadline = Date().addingTimeInterval(2)
    func visit(_ node: AXUIElement, _ depth: Int) -> AXUIElement? {
      budget -= 1
      guard budget >= 0, depth < 24, Date() < deadline, frame(node)?.contains(point) != false else {
        return nil
      }
      for child in children(node).reversed() {
        if let found = visit(child, depth + 1) { return found }
      }
      return frame(node)?.contains(point) == true && predicate(node) ? node : nil
    }
    return visit(element, 0)
  }
  init(_ id: UInt32) throws {
    let value = try info(id)
    self.id = id
    pid = value["pid"] as! Int32
    started = value["started"] as! Double
    let root = AXUIElementCreateApplication(pid)
    AXUIElementSetMessagingTimeout(root, 0.5)
    guard let items = attribute(root, kAXWindowsAttribute) as? [AXUIElement],
      let found = items.first(where: { windowID($0) == id })
    else { throw Failure(message: "Cannot bind an exact accessible window") }
    element = found
    AXUIElementSetMessagingTimeout(found, 0.5)
  }
  func validate(_ allowMinimized: Bool = false) throws -> [String: Any] {
    let value = try info(id)
    try require(
      value["pid"] as? Int32 == pid && value["started"] as? Double == started
        && windowID(element) == id, "Window identity changed; discover again")
    try require(
      allowMinimized || attribute(element, kAXMinimizedAttribute) as? Bool != true,
      "Minimized window is not live; restore it before observing")
    return value
  }
  func editable(_ point: CGPoint) -> AXUIElement? {
    matching(
      point,
      { node in
        writable(node, kAXValueAttribute)
          && text(node, kAXSubroleAttribute) != kAXSecureTextFieldSubrole
          && [kAXTextFieldRole, kAXTextAreaRole, kAXComboBoxRole].contains(
            text(node, kAXRoleAttribute))
      })
  }
  func pressable(_ point: CGPoint) -> AXUIElement? {
    matching(point, { actions($0).contains(kAXPressAction) })
  }
  func selected() throws -> AXUIElement {
    guard let receiver = receiver, let owner = attribute(receiver, kAXWindowAttribute),
      CFEqual(owner, element), attribute(receiver, kAXValueAttribute) != nil
    else { throw Failure(message: "Select a supported editable control first") }
    return receiver
  }
  func insert(_ value: String) throws {
    let receiver = try selected()
    let previous = text(receiver, kAXValueAttribute) as NSString
    guard let selection = attribute(receiver, kAXSelectedTextRangeAttribute),
      CFGetTypeID(selection) == AXValueGetTypeID()
    else { throw Failure(message: "Editable selection unavailable") }
    var range = CFRange()
    try require(
      AXValueGetValue(selection as! AXValue, .cfRange, &range) && range.location >= 0
        && range.length >= 0 && range.location + range.length <= previous.length,
      "Editable selection changed")
    try set(
      receiver, kAXValueAttribute,
      previous.replacingCharacters(
        in: NSRange(location: range.location, length: range.length), with: value) as CFString)
    range = CFRange(location: range.location + (value as NSString).length, length: 0)
    if let next = AXValueCreate(.cfRange, &range) {
      try set(receiver, kAXSelectedTextRangeAttribute, next)
    }
  }
}
private var leases: [String: Lease] = [:]
private var effect = false
private typealias EffectApproval = @convention(c) (UnsafeRawPointer?) -> Bool
private var approveEffect: EffectApproval?
private var effectContext: UnsafeRawPointer?
private func requireAdmission() throws {
  try require(
    approveEffect?(effectContext) != false,
    "GUI operation cancelled, disabled or expired before native input")
}
private func setPoint(_ element: AXUIElement, _ point: CGPoint) throws {
  var value = point
  guard let boxed = AXValueCreate(.cgPoint, &value) else {
    throw Failure(message: "Invalid position")
  }
  try set(element, kAXPositionAttribute, boxed)
}
private func setSize(_ element: AXUIElement, _ size: CGSize) throws {
  var value = size
  guard let boxed = AXValueCreate(.cgSize, &value) else { throw Failure(message: "Invalid size") }
  try set(element, kAXSizeAttribute, boxed)
}
private func adjust(_ lease: Lease, _ request: [String: Any]) throws {
  let bounds = try readRect(lease.validate(true)["bounds"])
  let action = try string(request, "action")
  if action == "restore" {
    try require(writable(lease.element, kAXMinimizedAttribute), "Window cannot be restored")
    try set(lease.element, kAXMinimizedAttribute, kCFBooleanFalse)
    return
  }
  if action == "move" {
    try require(writable(lease.element, kAXPositionAttribute), "Window position is not writable")
    let point = try CGPoint(x: number(request, "x"), y: number(request, "y"))
    try require(abs(point.x) <= 100000 && abs(point.y) <= 100000, "Position outside desktop budget")
    try setPoint(lease.element, point)
    return
  }
  if action == "resize" {
    let size = try CGSize(width: number(request, "width"), height: number(request, "height"))
    try require(
      size.width >= 100 && size.height >= 100 && size.width <= 3840 && size.height <= 2160
        && size.width * size.height * 4 <= 8_000_000, "Size outside image budget")
    try require(writable(lease.element, kAXSizeAttribute), "Window size is not writable")
    try setSize(lease.element, size)
    return
  }
  try require(action == "fit", "Unknown adjustment")
  try require(
    writable(lease.element, kAXPositionAttribute) && writable(lease.element, kAXSizeAttribute),
    "Window does not accept fit")
  let screens = NSScreen.screens
  guard
    let screen = screens.first(where: {
      $0.frame.contains(CGPoint(x: bounds.midX, y: (screens.first?.frame.maxY ?? 0) - bounds.midY))
    }) ?? screens.first
  else { throw Failure(message: "Display unavailable") }
  let area = screen.visibleFrame
  let top = (screens.first?.frame.maxY ?? 0) - area.maxY
  let ratio = min(1.0, sqrt(8_000_000 / (area.width * area.height * 4)))
  try setSize(lease.element, CGSize(width: area.width * ratio, height: area.height * ratio))
  try setPoint(lease.element, CGPoint(x: area.minX, y: top))
}
private func launch(_ path: String) throws -> [String: Any] {
  let url = URL(fileURLWithPath: path).standardizedFileURL
  guard url.pathExtension == "app", let bundle = Bundle(url: url),
    let identifier = bundle.bundleIdentifier, !identifier.hasPrefix("io.daat-locus."),
    bundle.executableURL?.lastPathComponent.hasPrefix("daat-locus") != true
  else { throw Failure(message: "Application unavailable or excluded") }
  try require(
    NSRunningApplication.runningApplications(withBundleIdentifier: identifier).isEmpty,
    "Application already runs; discover its windows instead of relaunching")
  let configuration = NSWorkspace.OpenConfiguration()
  configuration.activates = false
  configuration.addsToRecentItems = false
  configuration.promptsUserIfNeeded = false
  configuration.allowsRunningApplicationSubstitution = false
  let semaphore = DispatchSemaphore(value: 0)
  var launched: NSRunningApplication?
  try requireAdmission()
  effect = true
  NSWorkspace.shared.openApplication(at: url, configuration: configuration) { application, _ in
    launched = application
    semaphore.signal()
  }
  try require(
    semaphore.wait(timeout: .now() + 5) == .success,
    "Launch outcome unknown; discover windows before continuing")
  guard let application = launched else {
    throw Failure(message: "Launch unconfirmed; do not replay")
  }
  let root = AXUIElementCreateApplication(application.processIdentifier)
  AXUIElementSetMessagingTimeout(root, 0.5)
  let deadline = Date().addingTimeInterval(4)
  var candidateCount = 0
  while Date() < deadline {
    // WindowServer also reports transient rendering surfaces. Only exact AX
    // windows can be acquired, so use that same boundary for launch discovery.
    let accessible = Set(
      (attribute(root, kAXWindowsAttribute) as? [AXUIElement] ?? [])
        .compactMap { windowID($0) })
    let candidates = windows().compactMap {
      ($0[kCGWindowNumber as String] as? UInt32).flatMap { try? info($0) }
    }.filter {
      $0["pid"] as? Int32 == application.processIdentifier
        && ($0["id"] as? UInt32).map { accessible.contains($0) } == true
    }
    candidateCount = candidates.count
    if candidates.count == 1 { return ["receipt": candidates[0]] }
    Thread.sleep(forTimeInterval: 0.1)
  }
  throw Failure(
    message:
      "Launch returned \(candidateCount) accessible windows for PID \(application.processIdentifier); discover windows, never replay launch"
  )
}
@available(macOS 14.0, *)
private func capture(_ lease: Lease, _ request: [String: Any]) throws -> [String: Any] {
  let receipt = try lease.validate()
  let bounds = try readRect(receipt["bounds"])
  let semaphore = DispatchSemaphore(value: 0)
  var image: CGImage?
  var failure: Error?
  SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: false) {
    content, error in
    guard let window = content?.windows.first(where: { $0.windowID == lease.id }) else {
      failure = error ?? Failure(message: "Window is not capturable")
      semaphore.signal()
      return
    }
    let config = SCStreamConfiguration()
    config.width = Int(bounds.width.rounded(.up)) * 2
    config.height = Int(bounds.height.rounded(.up)) * 2
    config.showsCursor = false
    config.ignoreShadowsSingleWindow = true
    if config.width * config.height > 8_000_000 {
      failure = Failure(message: "Window image exceeds pixel budget")
      semaphore.signal()
      return
    }
    SCScreenshotManager.captureImage(
      contentFilter: SCContentFilter(desktopIndependentWindow: window), configuration: config
    ) { value, error in
      image = value
      failure = error
      semaphore.signal()
    }
  }
  try require(semaphore.wait(timeout: .now() + 5) == .success, "Window capture timed out")
  if let failure = failure { throw failure }
  guard let full = image else { throw Failure(message: "No window image") }
  var region = CGRect(x: 0, y: 0, width: full.width, height: full.height)
  if let requested = request["region"], !(requested is NSNull) { region = try readRect(requested) }
  let scale = request["scale"] as? Int ?? 1
  try require(
    scale >= 1 && scale <= 4 && region.width > 0 && region.height > 0
      && CGRect(x: 0, y: 0, width: full.width, height: full.height).contains(region)
      && region.width * region.height * Double(scale * scale) <= 8_000_000, "Invalid crop or scale")
  guard let crop = full.cropping(to: region),
    let context = CGContext(
      data: nil, width: Int(region.width) * scale, height: Int(region.height) * scale,
      bitsPerComponent: 8, bytesPerRow: 0, space: CGColorSpaceCreateDeviceRGB(),
      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
  else { throw Failure(message: "Cannot allocate window image") }
  context.interpolationQuality = .none
  context.draw(crop, in: CGRect(x: 0, y: 0, width: context.width, height: context.height))
  guard let output = context.makeImage(),
    let png = NSBitmapImageRep(cgImage: output).representation(using: .png, properties: [:])
  else { throw Failure(message: "Cannot encode window image") }
  let after = try lease.validate()
  try require(
    try readRect(after["bounds"]) == bounds, "Window geometry changed during capture; observe again"
  )
  let controls = lease.controls()
  lease.snapshot = try lease.signature(controls)
  let mapped = controls.map { item -> [String: Any] in
    var item = item
    if let r = try? readRect(item["bounds"]) {
      item["bounds"] = rectJSON(
        CGRect(
          x: ((r.minX - bounds.minX) / bounds.width * Double(full.width) - region.minX)
            * Double(scale),
          y: ((r.minY - bounds.minY) / bounds.height * Double(full.height) - region.minY)
            * Double(scale), width: r.width / bounds.width * Double(full.width) * Double(scale),
          height: r.height / bounds.height * Double(full.height) * Double(scale)))
    }
    return item
  }
  var result: [String: Any] = [
    "receipt": receipt, "width": output.width, "height": output.height, "window_width": full.width,
    "window_height": full.height, "image": png.base64EncodedString(), "region": rectJSON(region),
    "scale": scale, "controls": mapped,
  ]
  if let receiver = lease.receiver {
    result["input_target"] = [
      "kind": "editable_control", "text": text(receiver, kAXValueAttribute),
    ]
  }
  return result
}
// Owner-only permission actions intentionally open system UI. Repeated TCC
// requests can be silent after a prior decision, so always provide the pane.
private func openPrivacyPane(_ pane: String) throws {
  let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(pane)")!
  let opened: Bool
  if Thread.isMainThread {
    opened = NSWorkspace.shared.open(url)
  } else {
    opened = DispatchQueue.main.sync { NSWorkspace.shared.open(url) }
  }
  try require(opened, "Cannot open System Settings")
}
@available(macOS 14.0, *)
private func perform(_ request: [String: Any]) throws -> [String: Any] {
  guard let op = request["op"] as? String else { throw Failure(message: "Missing operation") }
  if op == "permissions" {
    return [
      "accessibility": AXIsProcessTrusted(), "screen_recording": CGPreflightScreenCaptureAccess(),
    ]
  }
  if op == "request_accessibility" {
    _ = AXIsProcessTrustedWithOptions(
      [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary)
    try openPrivacyPane("Privacy_Accessibility")
    return [:]
  }
  if op == "request_screen_recording" {
    _ = CGRequestScreenCaptureAccess()
    try openPrivacyPane("Privacy_ScreenCapture")
    return [:]
  }
  if op == "release", let token = request["lease"] as? String {
    leases.removeValue(forKey: token)
    return [:]
  }
  try require(
    AXIsProcessTrusted() && CGPreflightScreenCaptureAccess(),
    "Enable Accessibility and Screen Recording in GUI settings")
  if op == "desktop_state" {
    return [
      "pid": NSWorkspace.shared.frontmostApplication?.processIdentifier ?? 0,
      "cursor": rectJSON(CGRect(origin: CGEvent(source: nil)?.location ?? .zero, size: .zero)),
    ]
  }
  if op == "launch" { return try launch(string(request, "path")) }
  if op == "list_apps" {
    let running = NSWorkspace.shared.runningApplications.filter {
      !excluded($0) && $0.activationPolicy == .regular
    }
    var found: [String: [String: Any]] = [:]
    for application in running {
      if let url = application.bundleURL {
        found[url.path] = [
          "path": url.path,
          "name": application.localizedName ?? url.deletingPathExtension().lastPathComponent,
          "bundle": application.bundleIdentifier ?? "",
        ]
      }
    }
    for parent in ["/Applications", "/System/Applications", NSHomeDirectory() + "/Applications"] {
      for url
        in (try? FileManager.default.contentsOfDirectory(
          at: URL(fileURLWithPath: parent), includingPropertiesForKeys: nil)) ?? []
      where url.pathExtension == "app" {
        let identifier = Bundle(url: url)?.bundleIdentifier ?? ""
        if !identifier.hasPrefix("io.daat-locus.") {
          found[url.path] = [
            "path": url.path, "name": url.deletingPathExtension().lastPathComponent,
            "bundle": identifier,
          ]
        }
      }
    }
    return [
      "apps": Array(found.values).sorted { ($0["path"] as! String) < ($1["path"] as! String) }
    ]
  }
  if op == "list_windows" {
    return [
      "windows": windows().compactMap {
        ($0[kCGWindowNumber as String] as? UInt32).flatMap { try? info($0) }
      }
    ]
  }
  if op == "bind" {
    let id = try number(request, "id")
    try require(
      id >= 1 && id <= Double(UInt32.max) && id.rounded() == id, "Invalid window identity")
    let lease = try Lease(UInt32(id))
    let token = UUID().uuidString
    leases[token] = lease
    return ["lease": token, "receipt": try lease.validate()]
  }
  guard let token = request["lease"] as? String, let lease = leases[token] else {
    throw Failure(message: "Unknown window lease")
  }
  if op == "release" {
    leases.removeValue(forKey: token)
    return [:]
  }
  let receipt = try lease.validate(op == "adjust_window")
  if op == "describe" { return ["receipt": receipt] }
  if let expected = request["bounds"] {
    try require(
      try readRect(expected) == readRect(receipt["bounds"]),
      "Window geometry changed; observe again")
  }
  if op == "capture" { return try capture(lease, request) }
  if request["check_snapshot"] as? Bool == true {
    try require(
      lease.snapshot != nil && lease.signature(lease.controls()) == lease.snapshot,
      "Window contents changed; observe again")
  }
  lease.snapshot = nil
  if op == "adjust_window" {
    try adjust(lease, request)
    lease.receiver = nil
    return [:]
  }
  if op == "scroll" {
    let point = try CGPoint(x: number(request, "x"), y: number(request, "y"))
    let dx = try number(request, "delta_x")
    let dy = try number(request, "delta_y")
    try require(
      abs(dx) <= 100 && abs(dy) <= 100 && (dx != 0 || dy != 0), "Scroll exceeds dispatch budget")
    guard let area = lease.matching(point, { text($0, kAXRoleAttribute) == kAXScrollAreaRole })
    else { throw Failure(message: "No accessible scroll area at point") }
    var changes: [(AXUIElement, Double)] = []
    for (delta, key) in [
      (dx, kAXHorizontalScrollBarAttribute), (dy, kAXVerticalScrollBarAttribute),
    ] where delta != 0 {
      guard let raw = attribute(area, key), CFGetTypeID(raw) == AXUIElementGetTypeID() else {
        throw Failure(message: "Requested scroll axis unavailable")
      }
      let bar = raw as! AXUIElement
      guard writable(bar, kAXValueAttribute),
        let previous = attribute(bar, kAXValueAttribute) as? NSNumber
      else { throw Failure(message: "Scroll bar not writable") }
      changes.append((bar, min(1, max(0, previous.doubleValue + delta * 0.1))))
    }
    for (bar, value) in changes {
      try set(bar, kAXValueAttribute, NSNumber(value: value))
    }
    return [:]
  }
  if op == "drag" {
    guard let path = request["path"] as? [[String: Any]], let first = path.first,
      let last = path.last, path.count >= 2, path.count <= 256
    else { throw Failure(message: "Invalid drag path") }
    let start = try CGPoint(x: number(first, "x"), y: number(first, "y"))
    let end = try CGPoint(x: number(last, "x"), y: number(last, "y"))
    guard let target = lease.matching(start, { text($0, kAXRoleAttribute) == kAXSliderRole }),
      let bounds = frame(target), bounds.contains(end), writable(target, kAXValueAttribute),
      let min = attribute(target, kAXMinValueAttribute) as? NSNumber,
      let max = attribute(target, kAXMaxValueAttribute) as? NSNumber
    else { throw Failure(message: "Drag has no background-safe slider route") }
    let vertical = text(target, kAXOrientationAttribute) == kAXVerticalOrientationValue
    let ratio =
      vertical ? (bounds.maxY - end.y) / bounds.height : (end.x - bounds.minX) / bounds.width
    try set(
      target, kAXValueAttribute,
      NSNumber(value: min.doubleValue + ratio * (max.doubleValue - min.doubleValue)))
    return [:]
  }
  if op == "click" {
    try require(
      request["button"] as? String == "left" && request["click_count"] as? Int == 1,
      "Only single left clicks are available on this surface")
    let point = try CGPoint(x: number(request, "x"), y: number(request, "y"))
    if let receiver = lease.editable(point) {
      lease.receiver = receiver
      return [:]
    }
    guard let target = lease.pressable(point) else {
      throw Failure(message: "No background-safe action at this point")
    }
    try requireAdmission()
    effect = true
    try require(
      AXUIElementPerformAction(target, kAXPressAction as CFString) == .success,
      "Control action was not confirmed")
    lease.receiver = nil
    return [:]
  }
  if op == "type_text" {
    try lease.insert(string(request, "text"))
    return [:]
  }
  if op == "press_key" {
    let receiver = try lease.selected()
    let key = try string(request, "key")
    var range = CFRange()
    guard let current = attribute(receiver, kAXSelectedTextRangeAttribute),
      CFGetTypeID(current) == AXValueGetTypeID(),
      AXValueGetValue(current as! AXValue, .cfRange, &range)
    else { throw Failure(message: "Selection unavailable") }
    if ["Super+a", "Cmd+a", "Command+a"].contains(key) {
      range = CFRange(location: 0, length: (text(receiver, kAXValueAttribute) as NSString).length)
    } else if key == "Backspace" {
      if range.length == 0 && range.location > 0 {
        let string = text(receiver, kAXValueAttribute) as NSString
        let removed = string.rangeOfComposedCharacterSequence(at: range.location - 1)
        range = CFRange(location: removed.location, length: removed.length)
      }
      try set(receiver, kAXSelectedTextRangeAttribute, AXValueCreate(.cfRange, &range)!)
      try lease.insert("")
      return [:]
    } else if key == "ArrowLeft" {
      let value = text(receiver, kAXValueAttribute) as NSString
      range = CFRange(
        location: range.length > 0
          ? range.location
          : (range.location > 0
            ? value.rangeOfComposedCharacterSequence(at: range.location - 1).location : 0),
        length: 0)
    } else if key == "ArrowRight" {
      let value = text(receiver, kAXValueAttribute) as NSString
      let end = range.location + range.length
      let next =
        end < value.length
        ? NSMaxRange(value.rangeOfComposedCharacterSequence(at: end)) : value.length
      range = CFRange(location: range.length > 0 ? end : next, length: 0)
    } else {
      throw Failure(message: "Key has no background-safe route on this surface")
    }
    try set(receiver, kAXSelectedTextRangeAttribute, AXValueCreate(.cfRange, &range)!)
    return [:]
  }
  if op == "close" {
    guard let close = attribute(lease.element, kAXCloseButtonAttribute),
      CFGetTypeID(close) == AXUIElementGetTypeID()
    else { throw Failure(message: "Window close control unavailable") }
    try requireAdmission()
    effect = true
    try require(
      AXUIElementPerformAction(close as! AXUIElement, kAXPressAction as CFString) == .success,
      "Window close unconfirmed")
    leases.removeValue(forKey: token)
    return [:]
  }
  throw Failure(message: "Operation has no background-safe route")
}
@_cdecl("dl_mac_request")
public func dlMacRequest(
  _ input: UnsafePointer<CChar>, _ permit: @escaping @convention(c) (UnsafeRawPointer?) -> Bool,
  _ context: UnsafeRawPointer?
) -> UnsafeMutablePointer<CChar>? {
  autoreleasepool {
    approveEffect = permit
    effectContext = context
    defer {
      approveEffect = nil
      effectContext = nil
    }
    effect = false
    let foreground = NSWorkspace.shared.frontmostApplication?.processIdentifier
    let cursor = CGEvent(source: nil)?.location
    do {
      guard
        let request = try JSONSerialization.jsonObject(with: Data(String(cString: input).utf8))
          as? [String: Any]
      else { throw Failure(message: "Invalid native request") }
      guard #available(macOS 14.0, *) else {
        throw Failure(message: "Background GUI operation requires macOS 14 or newer")
      }
      let result = try perform(request)
      if request["op"] as? String != "request_accessibility"
        && request["op"] as? String != "request_screen_recording"
      {
        try require(
          NSWorkspace.shared.frontmostApplication?.processIdentifier == foreground
            && CGEvent(source: nil)?.location == cursor,
          "Desktop state changed during operation; observe before continuing")
      }
      let data = try JSONSerialization.data(withJSONObject: ["result": result, "effect": effect])
      return strdup(String(decoding: data, as: UTF8.self))
    } catch {
      let message =
        (error as? Failure)?.message ?? "Native operation unavailable; observe before continuing"
      let data = try! JSONSerialization.data(withJSONObject: ["error": message, "effect": effect])
      return strdup(String(decoding: data, as: UTF8.self))
    }
  }
}
