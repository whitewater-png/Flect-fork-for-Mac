import SwiftUI
import FlectCore

/// The settings that matter most; everything else keeps its default (or whatever the file already holds).
@MainActor struct SettingsView: View {
    let model: AppModel
    @State private var language: String
    @State private var delete: String
    @State private var closeOk: Bool
    @State private var closeFail: Bool
    @State private var closeCancel: Bool
    @State private var ocrLang: String
    @State private var ocrOutput: String
    @State private var saved = false
    @State private var error: String?

    init(model: AppModel) {
        self.model = model
        let s = model.settings
        _language = State(initialValue: s.string("General", "Language", default: "System"))
        _delete = State(initialValue: s.string("Folder", "DeleteMethod", default: "RecycleBin"))
        _closeOk = State(initialValue: s.closeOnSuccess)
        _closeFail = State(initialValue: s.closeOnFailure)
        _closeCancel = State(initialValue: s.closeOnCancel)
        _ocrLang = State(initialValue: s.string("Ocr", "Language"))
        _ocrOutput = State(initialValue: s.string("Ocr", "Output", default: "TextFile"))
    }

    var body: some View {
        let loc = model.loc
        Form {
            Text(loc.t("app.noTask")).font(.caption).foregroundStyle(.secondary)
            Picker(loc.t("settings.general.language"), selection: $language) {
                Text(loc.t("settings.general.language.system")).tag("System")
                Text(loc.t("settings.general.language.en")).tag("English")
                Text(loc.t("settings.general.language.ja")).tag("Japanese")
                Text(loc.t("settings.general.language.zh")).tag("ChineseSimplified")
            }
            Picker(loc.t("settings.folder.delete"), selection: $delete) {
                Text(loc.t("settings.folder.delete.recycle")).tag("RecycleBin")
                Text(loc.t("settings.folder.delete.permanent")).tag("Permanent")
            }
            if delete == "Permanent" { Text(loc.t("settings.folder.delete.warning")).foregroundStyle(.red).font(.caption) }
            Toggle(loc.t("settings.general.dialog.success"), isOn: $closeOk)
            Toggle(loc.t("settings.general.dialog.failure"), isOn: $closeFail)
            Toggle(loc.t("settings.general.dialog.cancel"), isOn: $closeCancel)
            Picker(loc.t("settings.ocr.output"), selection: $ocrOutput) {
                Text(loc.t("settings.ocr.output.file")).tag("TextFile")
                Text(loc.t("settings.ocr.output.clipboard")).tag("Clipboard")
                Text(loc.t("settings.ocr.output.show")).tag("ShowOnly")
            }
            TextField(loc.t("settings.ocr.language"), text: $ocrLang, prompt: Text("ja-JP, en-US"))
            HStack {
                Button(loc.t("common.save")) { save() }.keyboardShortcut(.defaultAction)
                Button(loc.t("common.close")) { model.close() }
                if saved { Text(loc.t("common.ok")).foregroundStyle(.green) }
                if let e = error { Text(e).foregroundStyle(.red) }
            }
        }.padding(20)
    }

    private func save() {
        let s = model.settings
        s.set(["General", "Language"], language)
        s.set(["Folder", "DeleteMethod"], delete)
        s.set(["General", "CloseOnSuccess"], closeOk)
        s.set(["General", "CloseOnFailure"], closeFail)
        s.set(["General", "CloseOnCancel"], closeCancel)
        s.set(["Ocr", "Language"], ocrLang)
        s.set(["Ocr", "Output"], ocrOutput)
        do { try s.save(); saved = true; error = nil; Loc.shared.use(s.language) }
        catch { self.error = "\(error)" }
    }
}
