import java.util.zip.ZipFile
import javax.xml.XMLConstants
import javax.xml.parsers.DocumentBuilderFactory
import javax.xml.transform.TransformerFactory
import javax.xml.transform.dom.DOMSource
import javax.xml.transform.stream.StreamResult
import org.gradle.api.DefaultTask
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.file.RegularFileProperty
import org.gradle.api.tasks.InputFile
import org.gradle.api.tasks.OutputDirectory
import org.gradle.api.tasks.PathSensitive
import org.gradle.api.tasks.PathSensitivity
import org.gradle.api.tasks.TaskAction
import org.w3c.dom.Element

// Android overlays whole XML resources; it does not merge domain-config entries.
// Compose from the resolved AAR so dependency updates also update its CRL hosts.
abstract class MergeNetworkSecurityConfig : DefaultTask() {
    @get:InputFile
    @get:PathSensitive(PathSensitivity.NONE)
    abstract val verifierArchive: RegularFileProperty

    @get:InputFile
    @get:PathSensitive(PathSensitivity.NONE)
    abstract val localPolicy: RegularFileProperty

    @get:OutputDirectory
    abstract val outputDirectory: DirectoryProperty

    @TaskAction
    fun merge() {
        val factory = DocumentBuilderFactory.newInstance().apply {
            setFeature("http://apache.org/xml/features/disallow-doctype-decl", true)
            setAttribute(XMLConstants.ACCESS_EXTERNAL_DTD, "")
            setAttribute(XMLConstants.ACCESS_EXTERNAL_SCHEMA, "")
        }
        val builder = factory.newDocumentBuilder()
        val upstream = ZipFile(verifierArchive.get().asFile).use { archive ->
            val entry = requireNotNull(archive.getEntry("res/xml/network_security_config.xml")) {
                "The Android TLS verifier AAR must supply its CRL network policy"
            }
            archive.getInputStream(entry).use { builder.parse(it) }
        }
        val local = builder.parse(localPolicy.get().asFile)
        for (document in listOf(upstream, local)) {
            require(document.documentElement.tagName == "network-security-config")
            val defaults = document.getElementsByTagName("base-config")
            require(defaults.length == 1 &&
                (defaults.item(0) as Element).getAttribute("cleartextTrafficPermitted") == "false"
            ) { "Network policy must deny cleartext by default" }
        }
        val domains = local.documentElement.childNodes
        for (index in 0 until domains.length) {
            val node = domains.item(index)
            if (node is Element && node.tagName == "domain-config") {
                upstream.documentElement.appendChild(upstream.importNode(node, true))
            }
        }
        val output = outputDirectory.file("xml/network_security_config.xml").get().asFile
        output.parentFile.mkdirs()
        TransformerFactory.newInstance().apply {
            setAttribute(XMLConstants.ACCESS_EXTERNAL_DTD, "")
            setAttribute(XMLConstants.ACCESS_EXTERNAL_STYLESHEET, "")
        }.newTransformer().transform(DOMSource(upstream), StreamResult(output))
    }
}
