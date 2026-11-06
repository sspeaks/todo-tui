import { Redirect, useLocalSearchParams } from "expo-router";
import { ItemDetail } from "../../components/ItemDetail";
import { useSession } from "../../session";

export default function ItemScreen() {
  const { id } = useLocalSearchParams<{ id: string }>();
  const { session } = useSession();
  if (session.state !== "ready" || !id) return <Redirect href="/" />;
  return <ItemDetail id={id} />;
}
