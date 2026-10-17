import java.io.*;
import java.nio.file.*;

import com.healthmarketscience.jackcess.Database;
import com.healthmarketscience.jackcess.DatabaseBuilder;
import com.healthmarketscience.jackcess.crypt.CryptCodecProvider;
import com.healthmarketscience.jackcess.crypt.InvalidCredentialsException;
import com.healthmarketscience.jackcess.util.MemFileChannel;

// Autopsy's EncryptionDetectionFileIngestModule MS Access branch.
public class AccessOracle {
    static String verdict(Path p) {
        try (InputStream in = new BufferedInputStream(Files.newInputStream(p))) {
            MemFileChannel ch = MemFileChannel.newChannel(in);
            DatabaseBuilder b = new DatabaseBuilder();
            b.setChannel(ch);
            b.setCodecProvider(new CryptCodecProvider());
            Database db;
            try {
                db = b.open();
            } catch (InvalidCredentialsException e) {
                return "true";
            } catch (Exception e) {
                return "false (" + e.getClass().getSimpleName() + ")";
            }
            return String.valueOf(db.getDatabasePassword() != null);
        } catch (Throwable t) {
            return "ERROR " + t;
        }
    }

    public static void main(String[] args) {
        for (String a : args) {
            System.out.println(Paths.get(a).getFileName() + "\t" + verdict(Paths.get(a)));
        }
    }
}
