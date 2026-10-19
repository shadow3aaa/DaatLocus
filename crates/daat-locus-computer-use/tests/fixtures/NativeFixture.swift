import AppKit

final class Fixture: NSObject, NSApplicationDelegate {
  var window: NSWindow!
  var count = 0
  var counter: NSTextField!
  func applicationDidFinishLaunching(_ notification: Notification) {
    window = NSWindow(
      contentRect: NSRect(x: 100, y: 100, width: 640, height: 480),
      styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
    window.title = "Daat Locus Native Fixture"
    window.identifier = NSUserInterfaceItemIdentifier("native-fixture")
    let editor = NSTextField(frame: NSRect(x: 30, y: 350, width: 580, height: 36))
    editor.identifier = NSUserInterfaceItemIdentifier("native-editor")
    editor.stringValue = "initial"
    editor.setAccessibilityIdentifier("native-editor")
    window.contentView!.addSubview(editor)
    let button = NSButton(frame: NSRect(x: 30, y: 290, width: 180, height: 36))
    button.title = "Increment"
    button.target = self
    button.action = #selector(increment)
    button.setAccessibilityIdentifier("native-increment")
    window.contentView!.addSubview(button)
    counter = NSTextField(labelWithString: "Count: 0")
    counter.frame = NSRect(x: 230, y: 290, width: 250, height: 36)
    counter.setAccessibilityIdentifier("native-counter")
    window.contentView!.addSubview(counter)
    let slider = NSSlider(
      value: 0, minValue: 0, maxValue: 100, target: self, action: #selector(slide))
    slider.frame = NSRect(x: 30, y: 220, width: 580, height: 32)
    slider.setAccessibilityIdentifier("native-slider")
    window.contentView!.addSubview(slider)
    let scroll = NSScrollView(frame: NSRect(x: 30, y: 30, width: 580, height: 160))
    scroll.hasVerticalScroller = true
    let content = NSTextView(frame: NSRect(x: 0, y: 0, width: 550, height: 2000))
    content.string = (0..<100).map { "Line \($0)" }.joined(separator: "\n")
    content.setAccessibilityIdentifier("native-document")
    scroll.documentView = content
    scroll.setAccessibilityIdentifier("native-scroll")
    window.contentView!.addSubview(scroll)
    window.makeKeyAndOrderFront(nil)
  }
  @objc func increment() {
    count += 1
    counter.stringValue = "Count: \(count)"
  }
  @objc func slide(_ sender: NSSlider) {
    counter.stringValue = "Slider: \(Int(sender.doubleValue))"
  }
  func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}
let application = NSApplication.shared
let delegate = Fixture()
application.delegate = delegate
application.setActivationPolicy(.regular)
application.run()
