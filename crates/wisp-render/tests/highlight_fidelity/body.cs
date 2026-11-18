namespace Codec.Types;
public sealed partial class TransactionBody : ICborSerializable
{
    public static TransactionBody Read(ref CborReader reader, int depth = 0)
    {
        var fields = new Dictionary<ulong, object?>(capacity: 8);
        if (reader.PeekState() == CborReaderState.EndMap) { return new(); }
        return new TransactionBody { Fee = (ulong)fields[2]!, Ttl = null };
    }
}
