// Generates the Encryption Detection fixtures with the libraries Autopsy
// uses (Apache POI 5.3, PDFBox 3.0.3 and Jackcess 4.0.7, all inside
// tika-app 3.0.0) and prints, for each, what Autopsy's detection path
// decides: Tika's MIME type and whether AutoDetectParser throws Tika's
// EncryptedDocumentException (Autopsy's "password protected" signal).
//
//   java -cp tika-app-3.0.0.jar MakeEncrypted.java OUT_DIR TEMPLATE.doc > oracle.tsv
//
// TEMPLATE.doc is any Word 97 document (Tika's testWORD.doc). Also prints
// "vector" lines: POI EncryptionInfo blobs (as stored in an Excel FILEPASS
// record) for each legacy mode and password, with POI's verdict on the
// default password "VelvetSweatshop".

import java.io.*;
import java.nio.file.*;
import java.util.*;

import org.apache.poi.hssf.record.FilePassRecord;
import org.apache.poi.hssf.record.crypto.Biff8EncryptionKey;
import org.apache.poi.hssf.usermodel.HSSFWorkbook;
import org.apache.poi.hslf.usermodel.HSLFSlideShow;
import org.apache.poi.hwpf.HWPFDocument;
import org.apache.poi.poifs.crypt.*;
import org.apache.poi.poifs.filesystem.POIFSFileSystem;
import org.apache.poi.xwpf.usermodel.XWPFDocument;
import org.apache.pdfbox.pdmodel.PDDocument;
import org.apache.pdfbox.pdmodel.PDPage;
import org.apache.pdfbox.pdmodel.encryption.AccessPermission;
import org.apache.pdfbox.pdmodel.encryption.StandardProtectionPolicy;
import org.apache.tika.Tika;
import org.apache.tika.exception.EncryptedDocumentException;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.parser.AutoDetectParser;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.sax.BodyContentHandler;

public class MakeEncrypted {
    static String hex(byte[] b) {
        StringBuilder s = new StringBuilder();
        for (byte x : b) s.append(String.format("%02x", x));
        return s.toString();
    }

    static void xls(Path out, String password) throws Exception {
        Biff8EncryptionKey.setCurrentUserPassword(password);
        try (HSSFWorkbook wb = new HSSFWorkbook()) {
            wb.createSheet("s").createRow(0).createCell(0).setCellValue("secret value");
            try (OutputStream os = Files.newOutputStream(out)) {
                wb.write(os);
            }
        } finally {
            Biff8EncryptionKey.setCurrentUserPassword(null);
        }
    }

    static void doc(Path template, Path out, String password) throws Exception {
        Biff8EncryptionKey.setCurrentUserPassword(password);
        try (InputStream in = Files.newInputStream(template);
             HWPFDocument d = new HWPFDocument(in);
             OutputStream os = Files.newOutputStream(out)) {
            d.write(os);
        } finally {
            Biff8EncryptionKey.setCurrentUserPassword(null);
        }
    }

    static void ppt(Path out, String password) throws Exception {
        Biff8EncryptionKey.setCurrentUserPassword(password);
        try (HSLFSlideShow s = new HSLFSlideShow()) {
            s.createSlide().addTitle().setText("secret slide");
            try (OutputStream os = Files.newOutputStream(out)) {
                s.write(os);
            }
        } finally {
            Biff8EncryptionKey.setCurrentUserPassword(null);
        }
    }

    static byte[] docx() throws Exception {
        try (XWPFDocument d = new XWPFDocument(); ByteArrayOutputStream b = new ByteArrayOutputStream()) {
            d.createParagraph().createRun().setText("secret text");
            d.write(b);
            return b.toByteArray();
        }
    }

    static void ooxmlProtected(Path out, EncryptionMode mode) throws Exception {
        try (POIFSFileSystem fs = new POIFSFileSystem()) {
            EncryptionInfo info = new EncryptionInfo(mode);
            Encryptor enc = info.getEncryptor();
            enc.confirmPassword("secret");
            try (OutputStream os = enc.getDataStream(fs)) {
                os.write(docx());
            }
            try (OutputStream os = Files.newOutputStream(out)) {
                fs.writeFilesystem(os);
            }
        }
    }

    static void pdf(Path out, String owner, String user, int keyBits, boolean aes) throws Exception {
        try (PDDocument d = new PDDocument()) {
            d.addPage(new PDPage());
            if (owner != null) {
                StandardProtectionPolicy p = new StandardProtectionPolicy(owner, user, new AccessPermission());
                p.setEncryptionKeyLength(keyBits);
                p.setPreferAES(aes);
                d.protect(p);
            }
            d.save(out.toFile());
        }
    }

    static String verdict(Path p) {
        AutoDetectParser parser = new AutoDetectParser();
        Metadata md = new Metadata();
        try (InputStream in = new BufferedInputStream(Files.newInputStream(p))) {
            parser.parse(in, new BodyContentHandler(-1), md, new ParseContext());
            return "OK";
        } catch (EncryptedDocumentException e) {
            return "ENCRYPTED";
        } catch (Throwable t) {
            return "ERROR " + t.getClass().getSimpleName();
        }
    }

    static void vector(EncryptionMode mode, String password) throws Exception {
        FilePassRecord rec = new FilePassRecord(mode);
        rec.getEncryptionInfo().getEncryptor().confirmPassword(password);
        byte[] all = rec.serialize();
        byte[] body = Arrays.copyOfRange(all, 4, all.length);
        // Re-read like HSSF does and ask POI about the default password.
        boolean ok = rec.getEncryptionInfo().getDecryptor().verifyPassword(Decryptor.DEFAULT_PASSWORD);
        System.out.println("vector\t" + mode + "\t" + password + "\t" + hex(body) + "\t" + ok);
    }

    public static void main(String[] args) throws Exception {
        Path dir = Paths.get(args[0]);
        Path template = Paths.get(args[1]);
        Files.createDirectories(dir);
        xls(dir.resolve("pw.xls"), "secret");
        xls(dir.resolve("default-pw.xls"), Decryptor.DEFAULT_PASSWORD);
        xls(dir.resolve("plain.xls"), null);
        doc(template, dir.resolve("pw.doc"), "secret");
        doc(template, dir.resolve("plain.doc"), null);
        ppt(dir.resolve("pw.ppt"), "secret");
        ppt(dir.resolve("plain.ppt"), null);
        Files.write(dir.resolve("plain.docx"), docx());
        ooxmlProtected(dir.resolve("pw-agile.docx"), EncryptionMode.agile);
        ooxmlProtected(dir.resolve("pw-standard.docx"), EncryptionMode.standard);
        pdf(dir.resolve("plain.pdf"), null, null, 0, false);
        pdf(dir.resolve("owner-only.pdf"), "owner", "", 128, false);
        pdf(dir.resolve("pw-rc4-40.pdf"), "owner", "secret", 40, false);
        pdf(dir.resolve("pw-rc4-128.pdf"), "owner", "secret", 128, false);
        pdf(dir.resolve("pw-aes-128.pdf"), "owner", "secret", 128, true);
        pdf(dir.resolve("pw-aes-256.pdf"), "owner", "secret", 256, true);
        pdf(dir.resolve("owner-only-aes-256.pdf"), "owner", "", 256, true);

        Tika tika = new Tika();
        File[] files = dir.toFile().listFiles();
        Arrays.sort(files);
        for (File f : files) {
            String mime = tika.detect(f);
            System.out.println("file\t" + f.getName() + "\t" + mime + "\t" + verdict(f.toPath()));
        }
        for (EncryptionMode m : new EncryptionMode[]{EncryptionMode.xor, EncryptionMode.binaryRC4, EncryptionMode.cryptoAPI}) {
            vector(m, "secret");
            vector(m, Decryptor.DEFAULT_PASSWORD);
        }
    }
}
