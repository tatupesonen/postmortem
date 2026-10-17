// Differential-test oracle: real Apache Tika 3.0.0 on the same paths as
// examples/tika_oracle.rs. Reads paths from stdin; prints
// path \t detect(stream) \t detect(stream, name). The stream is not
// file-backed, like Autopsy's ReadContentInputStream.
//
//   java -cp tika-app-3.0.0.jar TikaOracle.java < paths.txt > tika.tsv

import java.io.BufferedReader;
import java.io.File;
import java.io.FileInputStream;
import java.io.FilterInputStream;
import java.io.InputStreamReader;
import java.io.PrintStream;
import java.nio.charset.StandardCharsets;

import org.apache.tika.Tika;
import org.apache.tika.io.TikaInputStream;

public class TikaOracle {
    public static void main(String[] args) throws Exception {
        Tika tika = new Tika();
        BufferedReader in = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8));
        PrintStream out = new PrintStream(System.out, true, "UTF-8");
        String line;
        while ((line = in.readLine()) != null) {
            File f = new File(line);
            String p1;
            String p2;
            try (TikaInputStream tis = TikaInputStream.get(new FilterInputStream(new FileInputStream(f)) { })) {
                p1 = tika.detect(tis);
            } catch (Throwable e) {
                p1 = "ERROR";
            }
            try (TikaInputStream tis = TikaInputStream.get(new FilterInputStream(new FileInputStream(f)) { })) {
                p2 = tika.detect(tis, f.getName());
            } catch (Throwable e) {
                p2 = "ERROR";
            }
            out.println(line + "\t" + p1 + "\t" + p2);
        }
    }
}
